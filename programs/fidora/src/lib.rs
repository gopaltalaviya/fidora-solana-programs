//! # Fidora on Solana
//!
//! Escrow, bonds, reputation and spend caps for AI agents that hire each
//! other: the earlier edition of the protocol, ported rule for rule to Anchor.
//!
//! ## Money model
//! One SPL token vault, owned by the `config` PDA, holds everything. Every
//! token in it sits in exactly one bucket of [`state::Totals`] (invariant I1:
//! vault balance == totals.sum()). Payments between agents are internal
//! credits; tokens leave only through `withdraw`, `complete_unstake` and
//! `claim_fees`, each atomic with its CPI transfer.
//!
//! ## Keys
//! An agent's `owner` is the person; its `wallet` is a separate keypair the
//! agent signs jobs with. The program accepts the wallet for job methods
//! only — hire, accept, submit, approve, dispute, cancel — inside the
//! per-task and rolling 24 h caps. Payments, dispute bonds and the worker
//! bonds it locks on accept all count toward those caps. Every method that
//! moves value out of the vault, or changes caps, needs the owner's
//! signature. A stolen agent key can therefore spend at most its caps, only
//! on registered staked agents, and can never withdraw.

use anchor_lang::prelude::*;
use anchor_spl::token::{self, Mint, Token, TokenAccount, TransferChecked};

pub mod budget;
pub mod errors;
pub mod events;
pub mod state;

use errors::FidoraError;
use events::*;
use state::*;

declare_id!("5pND3A7Xqibq5h9XxY7YtuoJ5TomYKn79usirDBysNRg");

fn now_s() -> Result<u64> {
    Ok(Clock::get()?.unix_timestamp.max(0) as u64)
}

fn bps_of(amount: u64, bps: u16) -> u64 {
    (amount as u128 * bps as u128 / BPS) as u64
}

#[program]
pub mod fidora {
    use super::*;

    // ═══════════════════════════ setup and roles ═══════════════════════════

    pub fn initialize(
        ctx: Context<Initialize>,
        arbiter: Pubkey,
        pauser: Pubkey,
        fee_recipient: Pubkey,
        params: Params,
    ) -> Result<()> {
        check_params(&params)?;
        let c = &mut ctx.accounts.config;
        c.owner = ctx.accounts.owner.key();
        c.pending_owner = None;
        c.arbiter = arbiter;
        c.pauser = pauser;
        c.fee_recipient = fee_recipient;
        c.mint = ctx.accounts.mint.key();
        c.paused = false;
        c.params = params;
        c.next_agent_id = 1;
        c.next_job_id = 1;
        c.totals = Totals::default();
        c.bump = ctx.bumps.config;
        c.vault_bump = ctx.bumps.vault;
        Ok(())
    }

    /// Two-step ownership transfer, so a typo cannot hand the program away.
    pub fn propose_owner(ctx: Context<Admin>, new_owner: Pubkey) -> Result<()> {
        only_owner(&ctx.accounts.config, &ctx.accounts.signer)?;
        ctx.accounts.config.pending_owner = Some(new_owner);
        Ok(())
    }

    pub fn accept_owner(ctx: Context<Admin>) -> Result<()> {
        let caller = ctx.accounts.signer.key();
        let c = &mut ctx.accounts.config;
        require!(c.pending_owner == Some(caller), FidoraError::NotPendingOwner);
        c.owner = caller;
        c.pending_owner = None;
        emit!(RoleSet { role: "owner".into(), account: caller });
        Ok(())
    }

    pub fn set_arbiter(ctx: Context<Admin>, arbiter: Pubkey) -> Result<()> {
        only_owner(&ctx.accounts.config, &ctx.accounts.signer)?;
        ctx.accounts.config.arbiter = arbiter;
        emit!(RoleSet { role: "arbiter".into(), account: arbiter });
        Ok(())
    }

    pub fn set_pauser(ctx: Context<Admin>, pauser: Pubkey) -> Result<()> {
        only_owner(&ctx.accounts.config, &ctx.accounts.signer)?;
        ctx.accounts.config.pauser = pauser;
        emit!(RoleSet { role: "pauser".into(), account: pauser });
        Ok(())
    }

    pub fn set_fee_recipient(ctx: Context<Admin>, fee_recipient: Pubkey) -> Result<()> {
        only_owner(&ctx.accounts.config, &ctx.accounts.signer)?;
        ctx.accounts.config.fee_recipient = fee_recipient;
        emit!(RoleSet { role: "fee_recipient".into(), account: fee_recipient });
        Ok(())
    }

    /// New parameters apply to new jobs only: each job copies its windows,
    /// fee and bonds when it is created.
    pub fn set_params(ctx: Context<Admin>, params: Params) -> Result<()> {
        only_owner(&ctx.accounts.config, &ctx.accounts.signer)?;
        check_params(&params)?;
        ctx.accounts.config.params = params;
        emit!(RoleSet { role: "params".into(), account: ctx.accounts.signer.key() });
        Ok(())
    }

    /// Stops new hires only. Every exit stays open, so no money is trapped.
    pub fn pause(ctx: Context<Admin>) -> Result<()> {
        let caller = ctx.accounts.signer.key();
        let c = &mut ctx.accounts.config;
        require!(caller == c.pauser || caller == c.owner, FidoraError::NotPauser);
        c.paused = true;
        emit!(PausedSet { by: caller, paused: true });
        Ok(())
    }

    pub fn unpause(ctx: Context<Admin>) -> Result<()> {
        only_owner(&ctx.accounts.config, &ctx.accounts.signer)?;
        ctx.accounts.config.paused = false;
        emit!(PausedSet { by: ctx.accounts.signer.key(), paused: false });
        Ok(())
    }

    // ═══════════════════════════ registry ═══════════════════════════

    /// Register an agent; its owner signs and pays the account's rent. The
    /// wallet must then call `accept_wallet` itself: an agent cannot claim
    /// someone else's key.
    pub fn register_agent(
        ctx: Context<RegisterAgent>,
        wallet: Pubkey,
        card_uri: String,
        per_task_cap: Option<u64>,
        daily_cap: Option<u64>,
    ) -> Result<()> {
        require!(card_uri.len() <= MAX_CARD_URI, FidoraError::CardUriTooLong);
        let c = &mut ctx.accounts.config;
        let id = c.next_agent_id;
        c.next_agent_id += 1;
        let per_task = per_task_cap.unwrap_or(c.params.default_per_task_cap);
        let daily = daily_cap.unwrap_or(c.params.default_daily_cap);
        require!(per_task <= daily, FidoraError::InvalidCaps);

        let a = &mut ctx.accounts.agent;
        a.id = id;
        a.owner = ctx.accounts.owner.key();
        a.wallet = None;
        a.pending_wallet = Some(wallet);
        a.card_uri = card_uri.clone();
        a.frozen = false;
        a.registered_at = now_s()?;
        a.purse = Purse::new(per_task, daily);
        a.stake = Stake::default();
        a.worker_record = WorkerRecord::default();
        a.client_record = ClientRecord::default();
        a.bump = ctx.bumps.agent;

        emit!(AgentRegistered { agent_id: id, owner: a.owner, wallet, card_uri });
        emit!(CapsSet { agent_id: id, per_task_cap: per_task, daily_cap: daily });
        Ok(())
    }

    /// The pending wallet accepts, proving the key agrees to act for the agent.
    pub fn accept_wallet(ctx: Context<AcceptWallet>, agent_id: u64) -> Result<()> {
        let caller = ctx.accounts.wallet.key();
        let a = &mut ctx.accounts.agent;
        require!(a.id == agent_id, FidoraError::NoSuchAgent);
        require!(a.pending_wallet == Some(caller), FidoraError::NotPendingWallet);
        let idx = &mut ctx.accounts.index;
        require!(idx.agent_id == 0, FidoraError::WalletTaken);
        let previous = a.wallet.take();
        if let Some(old) = previous {
            // The old wallet's index is released (agent_id 0), so it may act
            // for another agent later.
            let prev = ctx
                .accounts
                .previous_index
                .as_mut()
                .ok_or(FidoraError::NotPendingWallet)?;
            let (expected, _) =
                Pubkey::find_program_address(&[WALLET_SEED, old.as_ref()], ctx.program_id);
            require_keys_eq!(prev.key(), expected, FidoraError::NotPendingWallet);
            prev.agent_id = 0;
        }
        a.wallet = Some(caller);
        a.pending_wallet = None;
        idx.agent_id = agent_id;
        idx.bump = ctx.bumps.index;
        emit!(WalletAccepted { agent_id, wallet: caller, previous });
        Ok(())
    }

    /// Owner proposes a new wallet (key rotation); the old one keeps working
    /// until the new one accepts. `None` withdraws a proposal.
    pub fn propose_wallet(ctx: Context<OwnerAgent>, wallet: Option<Pubkey>) -> Result<()> {
        let a = &mut ctx.accounts.agent;
        a.pending_wallet = wallet;
        emit!(WalletProposed { agent_id: a.id, wallet });
        Ok(())
    }

    pub fn set_card_uri(ctx: Context<OwnerAgent>, card_uri: String) -> Result<()> {
        require!(card_uri.len() <= MAX_CARD_URI, FidoraError::CardUriTooLong);
        ctx.accounts.agent.card_uri = card_uri;
        Ok(())
    }

    /// Kill switch: a frozen agent's wallet can do nothing and it cannot be
    /// hired. Exits that anyone may call still work, so open jobs still end.
    pub fn freeze_agent(ctx: Context<OwnerAgent>) -> Result<()> {
        let a = &mut ctx.accounts.agent;
        a.frozen = true;
        emit!(AgentFrozen { agent_id: a.id, frozen: true });
        Ok(())
    }

    pub fn unfreeze_agent(ctx: Context<OwnerAgent>) -> Result<()> {
        let a = &mut ctx.accounts.agent;
        a.frozen = false;
        emit!(AgentFrozen { agent_id: a.id, frozen: false });
        Ok(())
    }

    pub fn set_caps(ctx: Context<OwnerAgent>, per_task_cap: u64, daily_cap: u64) -> Result<()> {
        require!(per_task_cap <= daily_cap, FidoraError::InvalidCaps);
        let a = &mut ctx.accounts.agent;
        a.purse.per_task_cap = per_task_cap;
        a.purse.daily_cap = daily_cap;
        emit!(CapsSet { agent_id: a.id, per_task_cap, daily_cap });
        Ok(())
    }

    // ═══════════════════════════ money in ═══════════════════════════

    /// Anyone may add to an agent's spendable balance.
    pub fn fund(ctx: Context<Deposit>, amount: u64) -> Result<()> {
        require!(amount > 0, FidoraError::ZeroAmount);
        pay_in(&ctx, amount)?;
        let c = &mut ctx.accounts.config;
        let a = &mut ctx.accounts.agent;
        credit(c, a, amount);
        emit!(Funded { agent_id: a.id, from: ctx.accounts.payer.key(), amount, kind: 0 });
        Ok(())
    }

    /// Anyone may add to an agent's stake (makes it hireable, backs its bonds).
    pub fn stake(ctx: Context<Deposit>, amount: u64) -> Result<()> {
        require!(amount > 0, FidoraError::ZeroAmount);
        pay_in(&ctx, amount)?;
        let c = &mut ctx.accounts.config;
        let a = &mut ctx.accounts.agent;
        a.stake.available += amount;
        c.totals.stakes += amount;
        emit!(Funded { agent_id: a.id, from: ctx.accounts.payer.key(), amount, kind: 1 });
        Ok(())
    }

    // ═══════════════════════════ money out ═══════════════════════════

    /// Owner withdraws from the agent's balance, to any token account of the
    /// mint. Needs the OWNER's signature: a leaked agent key cannot.
    pub fn withdraw(ctx: Context<Withdraw>, amount: u64) -> Result<()> {
        require!(amount > 0, FidoraError::ZeroAmount);
        {
            let a = &mut ctx.accounts.agent;
            require!(a.purse.balance >= amount, FidoraError::InsufficientBalance);
            a.purse.balance -= amount;
            ctx.accounts.config.totals.balances -= amount;
        }
        pay_out(
            &ctx.accounts.token_program,
            &ctx.accounts.vault,
            &ctx.accounts.mint,
            &ctx.accounts.to,
            &ctx.accounts.config,
            amount,
        )?;
        emit!(Withdrawn {
            agent_id: ctx.accounts.agent.id,
            to: ctx.accounts.to.key(),
            amount
        });
        Ok(())
    }

    /// Anyone may push accumulated fees to the fee recipient's token account.
    pub fn claim_fees(ctx: Context<ClaimFees>) -> Result<()> {
        let amount = ctx.accounts.config.totals.fees_owed;
        require!(amount > 0, FidoraError::NothingOwed);
        require_keys_eq!(
            ctx.accounts.to.owner,
            ctx.accounts.config.fee_recipient,
            FidoraError::WrongRecipient
        );
        ctx.accounts.config.totals.fees_owed = 0;
        pay_out(
            &ctx.accounts.token_program,
            &ctx.accounts.vault,
            &ctx.accounts.mint,
            &ctx.accounts.to,
            &ctx.accounts.config,
            amount,
        )?;
        emit!(FeesClaimed { to: ctx.accounts.to.key(), amount });
        Ok(())
    }

    // ═══════════════════════════ stake ═══════════════════════════

    pub fn request_unstake(ctx: Context<OwnerStake>, amount: u64) -> Result<()> {
        require!(amount > 0, FidoraError::ZeroAmount);
        let delay = ctx.accounts.config.params.withdraw_delay;
        let a = &mut ctx.accounts.agent;
        require!(a.stake.pending == 0, FidoraError::WithdrawAlreadyPending);
        require!(a.stake.unlocked() >= amount, FidoraError::InsufficientBond);
        a.stake.available -= amount;
        a.stake.pending = amount;
        a.stake.unlock_at = now_s()? + delay;
        emit!(UnstakeRequested { agent_id: a.id, amount, unlock_at: a.stake.unlock_at });
        Ok(())
    }

    pub fn cancel_unstake(ctx: Context<OwnerStake>) -> Result<()> {
        let a = &mut ctx.accounts.agent;
        require!(a.stake.pending > 0, FidoraError::NothingPending);
        let amount = a.stake.pending;
        a.stake.available += amount;
        a.stake.pending = 0;
        a.stake.unlock_at = 0;
        emit!(UnstakeCancelled { agent_id: a.id, amount });
        Ok(())
    }

    /// Pays matured stake to the owner's own token account. It never passes
    /// through the agent's balance, which the agent key could spend.
    pub fn complete_unstake(ctx: Context<Withdraw>) -> Result<()> {
        let amount;
        {
            let a = &mut ctx.accounts.agent;
            require!(a.stake.pending > 0, FidoraError::NothingPending);
            require!(now_s()? >= a.stake.unlock_at, FidoraError::StillLocked);
            require_keys_eq!(ctx.accounts.to.owner, a.owner, FidoraError::WrongRecipient);
            amount = a.stake.pending;
            a.stake.pending = 0;
            a.stake.unlock_at = 0;
            ctx.accounts.config.totals.stakes -= amount;
        }
        pay_out(
            &ctx.accounts.token_program,
            &ctx.accounts.vault,
            &ctx.accounts.mint,
            &ctx.accounts.to,
            &ctx.accounts.config,
            amount,
        )?;
        emit!(Unstaked { agent_id: ctx.accounts.agent.id, amount });
        Ok(())
    }

    // ═══════════════════════════ escrow ═══════════════════════════

    /// Client hires worker; `amount` moves from the client's balance into
    /// escrow, under the client's caps. The client's wallet signs and pays
    /// the job account's rent.
    pub fn create_job(
        ctx: Context<CreateJob>,
        amount: u64,
        spec_hash: [u8; 32],
        accept_window: u64,
        work_window: u64,
    ) -> Result<()> {
        let c = &mut ctx.accounts.config;
        require!(!c.paused, FidoraError::Paused);
        let client = &mut ctx.accounts.client;
        let worker = &ctx.accounts.worker;
        validate_hire(c, client, worker, &ctx.accounts.wallet.key(), amount)?;
        require!(amount <= c.params.max_job_amount, FidoraError::AmountAboveMaximum);
        require!(accept_window > 0 && work_window > 0, FidoraError::WindowTooLong);
        require!(
            accept_window <= c.params.max_accept_window && work_window <= c.params.max_work_window,
            FidoraError::WindowTooLong
        );

        let now = now_s()?;
        spend(c, client, amount, now)?;
        c.totals.locked += amount;

        let id = c.next_job_id;
        c.next_job_id += 1;
        let fee = fee_of(&c.params, amount);
        let accept_deadline = now + accept_window;
        let j = &mut ctx.accounts.job;
        j.id = id;
        j.client = client.id;
        j.worker = worker.id;
        j.amount = amount;
        j.fee = fee;
        j.state = JobState::Created;
        j.accept_deadline = accept_deadline;
        // Absolute from the start, so the worker knows its real deadline
        // before accepting rather than after.
        j.work_deadline = accept_deadline + work_window;
        j.review_deadline = 0;
        j.dispute_deadline = 0;
        j.review_window = c.params.review_window;
        j.dispute_timeout = c.params.dispute_timeout;
        j.spec_hash = spec_hash;
        j.result_hash = None;
        j.worker_bond = bps_of(amount, c.params.bond_bps).max(c.params.min_bond);
        j.dispute_bond = bps_of(amount, c.params.dispute_bond_bps).max(c.params.min_dispute_bond);
        j.bond_locked = false;
        j.review_hash = None;
        j.outcome = None;
        j.created_at = now;
        j.bump = ctx.bumps.job;
        emit!(JobCreated {
            job_id: id,
            client: client.id,
            worker: worker.id,
            amount,
            fee,
            spec_hash,
            accept_deadline,
            work_deadline: j.work_deadline,
        });
        Ok(())
    }

    /// Worker accepts and locks its bond. Allowed while paused: it is not a
    /// new hire. Only that the unlocked stake covers the bond is re-checked.
    pub fn accept_job(ctx: Context<JobAction>) -> Result<()> {
        let j = &mut ctx.accounts.job;
        let w = &mut ctx.accounts.worker;
        only_wallet_of(w, &ctx.accounts.signer)?;
        require!(j.state == JobState::Created, FidoraError::InvalidState);
        require!(now_s()? <= j.accept_deadline, FidoraError::DeadlinePassed);
        let bond = j.worker_bond;
        if bond > 0 {
            require!(w.stake.unlocked() >= bond, FidoraError::InsufficientBond);
            // The bond counts toward the worker's own caps: a stolen wallet key
            // could otherwise accept jobs it never delivers and hand the whole
            // stake to an accomplice client through expire_undelivered.
            budget::charge(&mut w.purse, bond, now_s()?)?;
            w.stake.locked += bond;
            j.bond_locked = true;
        }
        j.state = JobState::Accepted;
        emit!(JobAccepted { job_id: j.id, worker: w.id, bond, work_deadline: j.work_deadline });
        Ok(())
    }

    pub fn submit_result(ctx: Context<JobAction>, result_hash: [u8; 32]) -> Result<()> {
        let j = &mut ctx.accounts.job;
        only_wallet_of(&ctx.accounts.worker, &ctx.accounts.signer)?;
        require!(j.state == JobState::Accepted, FidoraError::InvalidState);
        let now = now_s()?;
        require!(now <= j.work_deadline, FidoraError::DeadlinePassed);
        j.state = JobState::Submitted;
        j.review_deadline = now + j.review_window;
        j.result_hash = Some(result_hash);
        emit!(ResultSubmitted { job_id: j.id, result_hash, review_deadline: j.review_deadline });
        Ok(())
    }

    /// Client accepts the work. No deadline: it works after the review window
    /// too, until someone calls `auto_approve`.
    pub fn approve(ctx: Context<JobAction>, review_hash: Option<[u8; 32]>) -> Result<()> {
        let a = ctx.accounts;
        only_wallet_of(&a.client, &a.signer)?;
        require!(a.job.state == JobState::Submitted, FidoraError::InvalidState);
        a.job.review_hash = review_hash;
        a.client.client_record.paid += 1;
        settle(&mut a.config, &mut a.job, &mut a.worker, Outcome::Success, true);
        review_client(&a.job, &mut a.client, 100);
        Ok(())
    }

    /// Client disputes inside the review window, paying the dispute bond from
    /// its balance under its caps.
    pub fn dispute(ctx: Context<JobAction>, reason_hash: Option<[u8; 32]>) -> Result<()> {
        let a = ctx.accounts;
        only_wallet_of(&a.client, &a.signer)?;
        require!(a.job.state == JobState::Submitted, FidoraError::InvalidState);
        let now = now_s()?;
        require!(now <= a.job.review_deadline, FidoraError::DeadlinePassed);
        let bond = a.job.dispute_bond;
        if bond > 0 {
            spend(&mut a.config, &mut a.client, bond, now)?;
            a.config.totals.dispute_bonds += bond;
        }
        a.job.state = JobState::Disputed;
        a.job.dispute_deadline = now + a.job.dispute_timeout;
        a.job.review_hash = reason_hash;
        emit!(JobDisputed {
            job_id: a.job.id,
            bond,
            dispute_deadline: a.job.dispute_deadline,
            reason_hash
        });
        Ok(())
    }

    /// The arbiter rules. No deadline: it may rule until someone calls
    /// `expire_dispute`. The arbiter can be a Squads multisig vault.
    pub fn resolve_dispute(ctx: Context<JobAction>, for_worker: bool) -> Result<()> {
        let a = ctx.accounts;
        require_keys_eq!(a.signer.key(), a.config.arbiter, FidoraError::NotArbiter);
        require!(a.job.state == JobState::Disputed, FidoraError::InvalidState);
        let bond = a.job.dispute_bond;
        a.config.totals.dispute_bonds -= bond;
        emit!(DisputeResolved { job_id: a.job.id, for_worker, arbiter: a.signer.key() });
        if for_worker {
            a.client.client_record.disputes_rejected += 1;
            credit(&mut a.config, &mut a.worker, bond);
            if bond > 0 {
                emit!(DisputeBondSettled { job_id: a.job.id, to: a.worker.id, amount: bond });
            }
            settle(&mut a.config, &mut a.job, &mut a.worker, Outcome::Success, true);
            review_client(&a.job, &mut a.client, 0);
        } else {
            a.client.client_record.disputes_upheld += 1;
            credit(&mut a.config, &mut a.client, bond);
            if bond > 0 {
                emit!(DisputeBondSettled { job_id: a.job.id, to: a.client.id, amount: bond });
            }
            refund(&mut a.config, &mut a.job, &mut a.client, &mut a.worker, "dispute", Some(Outcome::DisputeLost));
            review_client(&a.job, &mut a.client, 100);
        }
        Ok(())
    }

    /// The worker refuses an offer it has not accepted: the client is refunded
    /// at once, and no bond is touched (it was never locked). This is the x402
    /// `cancel` leg of Fidora's escrow scheme: a paid call whose handler fails
    /// is declined, so the payer gets its money back in the same request.
    pub fn decline_job(ctx: Context<JobAction>) -> Result<()> {
        let a = ctx.accounts;
        only_wallet_of(&a.worker, &a.signer)?;
        require!(a.job.state == JobState::Created, FidoraError::InvalidState);
        refund(&mut a.config, &mut a.job, &mut a.client, &mut a.worker, "declined", None);
        Ok(())
    }

    /// Client withdraws an offer nobody accepted — even after the deadline.
    pub fn cancel(ctx: Context<JobAction>) -> Result<()> {
        let a = ctx.accounts;
        only_wallet_of(&a.client, &a.signer)?;
        require!(a.job.state == JobState::Created, FidoraError::InvalidState);
        refund(&mut a.config, &mut a.job, &mut a.client, &mut a.worker, "cancelled", None);
        Ok(())
    }

    // ── exits anyone may call (invariant I5: no open state lasts forever) ──

    pub fn expire_unaccepted(ctx: Context<JobAction>) -> Result<()> {
        let a = ctx.accounts;
        require!(a.job.state == JobState::Created, FidoraError::InvalidState);
        require!(now_s()? > a.job.accept_deadline, FidoraError::DeadlineNotPassed);
        refund(&mut a.config, &mut a.job, &mut a.client, &mut a.worker, "unaccepted", None);
        Ok(())
    }

    /// The worker accepted and never delivered: refund the client and give it
    /// the worker's bond.
    pub fn expire_undelivered(ctx: Context<JobAction>) -> Result<()> {
        let a = ctx.accounts;
        require!(a.job.state == JobState::Accepted, FidoraError::InvalidState);
        require!(now_s()? > a.job.work_deadline, FidoraError::DeadlineNotPassed);
        refund(&mut a.config, &mut a.job, &mut a.client, &mut a.worker, "undelivered", Some(Outcome::Failed));
        Ok(())
    }

    /// The client stayed silent through the review window: the worker is paid.
    pub fn auto_approve(ctx: Context<JobAction>) -> Result<()> {
        let a = ctx.accounts;
        require!(a.job.state == JobState::Submitted, FidoraError::InvalidState);
        require!(now_s()? > a.job.review_deadline, FidoraError::DeadlineNotPassed);
        a.client.client_record.silent += 1;
        settle(&mut a.config, &mut a.job, &mut a.worker, Outcome::Success, true);
        review_client(&a.job, &mut a.client, 50);
        Ok(())
    }

    /// The arbiter never ruled: the worker is paid, the client's bond comes
    /// back, nobody is rated.
    pub fn expire_dispute(ctx: Context<JobAction>) -> Result<()> {
        let a = ctx.accounts;
        require!(a.job.state == JobState::Disputed, FidoraError::InvalidState);
        require!(now_s()? > a.job.dispute_deadline, FidoraError::DeadlineNotPassed);
        let bond = a.job.dispute_bond;
        a.config.totals.dispute_bonds -= bond;
        credit(&mut a.config, &mut a.client, bond);
        if bond > 0 {
            emit!(DisputeBondSettled { job_id: a.job.id, to: a.client.id, amount: bond });
        }
        emit!(DisputeExpired { job_id: a.job.id });
        settle(&mut a.config, &mut a.job, &mut a.worker, Outcome::Unresolved, false);
        Ok(())
    }

    /// Small, instant payment with no escrow window, for trusted workers.
    pub fn direct_pay(ctx: Context<DirectPay>, amount: u64, spec_hash: [u8; 32]) -> Result<()> {
        let c = &mut ctx.accounts.config;
        require!(!c.paused, FidoraError::Paused);
        let client = &mut ctx.accounts.client;
        let worker = &mut ctx.accounts.worker;
        validate_hire(c, client, worker, &ctx.accounts.wallet.key(), amount)?;
        require!(amount <= c.params.fast_path_max, FidoraError::AmountTooLargeForFastPath);

        let now = now_s()?;
        spend(c, client, amount, now)?;
        let fee = fee_of(&c.params, amount);
        credit(c, worker, amount - fee);
        c.totals.fees_owed += fee;

        let id = c.next_job_id;
        c.next_job_id += 1;
        let j = &mut ctx.accounts.job;
        j.id = id;
        j.client = client.id;
        j.worker = worker.id;
        j.amount = amount;
        j.fee = fee;
        j.state = JobState::Settled;
        j.accept_deadline = now;
        j.work_deadline = now;
        j.review_deadline = now;
        j.dispute_deadline = 0;
        j.review_window = 0;
        j.dispute_timeout = 0;
        j.spec_hash = spec_hash;
        j.result_hash = None;
        j.worker_bond = 0;
        j.dispute_bond = 0;
        j.bond_locked = false;
        j.review_hash = None;
        j.outcome = Some(Outcome::Success);
        j.created_at = now;
        j.bump = ctx.bumps.job;
        worker.worker_record.completed += 1;
        client.client_record.paid += 1;
        emit!(DirectPaid { job_id: id, client: client.id, worker: worker.id, amount, fee, spec_hash });
        emit!(JobSettled { job_id: id, worker: worker.id, paid: amount - fee, fee, outcome: Outcome::Success });
        review_worker(j, worker, 100);
        review_client(j, client, 100);
        Ok(())
    }
}

// ═══════════════════════════ accounts ═══════════════════════════

#[derive(Accounts)]
pub struct Initialize<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,
    #[account(init, payer = owner, space = 8 + Config::INIT_SPACE, seeds = [CONFIG_SEED], bump)]
    pub config: Account<'info, Config>,
    pub mint: Account<'info, Mint>,
    #[account(
        init,
        payer = owner,
        seeds = [VAULT_SEED],
        bump,
        token::mint = mint,
        token::authority = config,
    )]
    pub vault: Account<'info, TokenAccount>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct Admin<'info> {
    pub signer: Signer<'info>,
    #[account(mut, seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, Config>,
}

#[derive(Accounts)]
pub struct RegisterAgent<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,
    #[account(mut, seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, Config>,
    #[account(
        init,
        payer = owner,
        space = 8 + Agent::INIT_SPACE,
        seeds = [AGENT_SEED, config.next_agent_id.to_le_bytes().as_ref()],
        bump,
    )]
    pub agent: Account<'info, Agent>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(agent_id: u64)]
pub struct AcceptWallet<'info> {
    #[account(mut)]
    pub wallet: Signer<'info>,
    #[account(mut, seeds = [AGENT_SEED, agent_id.to_le_bytes().as_ref()], bump = agent.bump)]
    pub agent: Account<'info, Agent>,
    #[account(
        init_if_needed,
        payer = wallet,
        space = 8 + WalletIndex::INIT_SPACE,
        seeds = [WALLET_SEED, wallet.key().as_ref()],
        bump,
    )]
    pub index: Account<'info, WalletIndex>,
    /// The previous wallet's index, when the agent is rotating its wallet.
    #[account(mut)]
    pub previous_index: Option<Account<'info, WalletIndex>>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct OwnerAgent<'info> {
    pub owner: Signer<'info>,
    #[account(mut, has_one = owner @ FidoraError::NotAgentOwner)]
    pub agent: Account<'info, Agent>,
}

#[derive(Accounts)]
pub struct OwnerStake<'info> {
    pub owner: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, Config>,
    #[account(mut, has_one = owner @ FidoraError::NotAgentOwner)]
    pub agent: Account<'info, Agent>,
}

#[derive(Accounts)]
pub struct Deposit<'info> {
    pub payer: Signer<'info>,
    #[account(mut, seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, Config>,
    #[account(mut)]
    pub agent: Account<'info, Agent>,
    #[account(mut, token::mint = mint, token::authority = payer)]
    pub from: Account<'info, TokenAccount>,
    #[account(mut, seeds = [VAULT_SEED], bump = config.vault_bump)]
    pub vault: Account<'info, TokenAccount>,
    #[account(address = config.mint @ FidoraError::WrongMint)]
    pub mint: Account<'info, Mint>,
    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct Withdraw<'info> {
    pub owner: Signer<'info>,
    #[account(mut, seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, Config>,
    #[account(mut, has_one = owner @ FidoraError::NotAgentOwner)]
    pub agent: Account<'info, Agent>,
    #[account(mut, token::mint = mint)]
    pub to: Account<'info, TokenAccount>,
    #[account(mut, seeds = [VAULT_SEED], bump = config.vault_bump)]
    pub vault: Account<'info, TokenAccount>,
    #[account(address = config.mint @ FidoraError::WrongMint)]
    pub mint: Account<'info, Mint>,
    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct ClaimFees<'info> {
    pub signer: Signer<'info>,
    #[account(mut, seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, Config>,
    #[account(mut, token::mint = mint)]
    pub to: Account<'info, TokenAccount>,
    #[account(mut, seeds = [VAULT_SEED], bump = config.vault_bump)]
    pub vault: Account<'info, TokenAccount>,
    #[account(address = config.mint @ FidoraError::WrongMint)]
    pub mint: Account<'info, Mint>,
    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct CreateJob<'info> {
    /// The client agent's wallet: signs, and pays the job account's rent.
    #[account(mut)]
    pub wallet: Signer<'info>,
    #[account(mut, seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, Config>,
    #[account(mut)]
    pub client: Account<'info, Agent>,
    pub worker: Account<'info, Agent>,
    #[account(
        init,
        payer = wallet,
        space = 8 + Job::INIT_SPACE,
        seeds = [JOB_SEED, config.next_job_id.to_le_bytes().as_ref()],
        bump,
    )]
    pub job: Account<'info, Job>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct DirectPay<'info> {
    #[account(mut)]
    pub wallet: Signer<'info>,
    #[account(mut, seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, Config>,
    #[account(mut)]
    pub client: Account<'info, Agent>,
    #[account(mut)]
    pub worker: Account<'info, Agent>,
    #[account(
        init,
        payer = wallet,
        space = 8 + Job::INIT_SPACE,
        seeds = [JOB_SEED, config.next_job_id.to_le_bytes().as_ref()],
        bump,
    )]
    pub job: Account<'info, Job>,
    pub system_program: Program<'info, System>,
}

/// Every step after a hire: the job and both of its agents. Who may act is
/// checked in the handler (the worker's wallet, the client's wallet, the
/// arbiter, or anyone for the exits).
#[derive(Accounts)]
pub struct JobAction<'info> {
    pub signer: Signer<'info>,
    #[account(mut, seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, Config>,
    #[account(mut, seeds = [JOB_SEED, job.id.to_le_bytes().as_ref()], bump = job.bump)]
    pub job: Account<'info, Job>,
    #[account(mut, constraint = client.id == job.client @ FidoraError::NoSuchAgent)]
    pub client: Account<'info, Agent>,
    #[account(mut, constraint = worker.id == job.worker @ FidoraError::NoSuchAgent)]
    pub worker: Account<'info, Agent>,
}

// ═══════════════════════════ internals ═══════════════════════════

fn only_owner(c: &Config, signer: &Signer) -> Result<()> {
    require_keys_eq!(signer.key(), c.owner, FidoraError::NotOwner);
    Ok(())
}

/// The signer must be the agent's accepted wallet, and the agent not frozen.
fn only_wallet_of(a: &Agent, signer: &Signer) -> Result<()> {
    require!(a.wallet == Some(signer.key()), FidoraError::NotAgentWallet);
    require!(!a.frozen, FidoraError::AgentFrozen);
    Ok(())
}

/// The earlier editions' hire checks, in the same order.
fn validate_hire(c: &Config, client: &Agent, worker: &Agent, signer: &Pubkey, amount: u64) -> Result<()> {
    require!(amount > 0, FidoraError::ZeroAmount);
    require!(amount >= c.params.min_job_amount, FidoraError::AmountBelowMinimum);
    require!(client.id != worker.id, FidoraError::SelfDealing);
    require!(client.wallet == Some(*signer), FidoraError::NotAgentWallet);
    require!(!client.frozen, FidoraError::AgentFrozen);
    let worker_wallet = match (worker.wallet, worker.frozen) {
        (Some(w), false) => w,
        _ => return err!(FidoraError::NotHireable),
    };
    require!(worker_wallet != *signer, FidoraError::SelfDealing);
    require!(client.owner != worker.owner, FidoraError::SameOwner);
    require!(worker.stake.unlocked() >= c.params.min_stake, FidoraError::NotHireable);
    Ok(())
}

/// Take `amount` from the agent's balance under its caps.
fn spend(c: &mut Config, a: &mut Agent, amount: u64, now: u64) -> Result<()> {
    require!(a.purse.balance >= amount, FidoraError::InsufficientBalance);
    budget::charge(&mut a.purse, amount, now)?;
    a.purse.balance -= amount;
    c.totals.balances -= amount;
    Ok(())
}

fn credit(c: &mut Config, a: &mut Agent, amount: u64) {
    if amount == 0 {
        return;
    }
    a.purse.balance += amount;
    c.totals.balances += amount;
}

fn fee_of(p: &Params, amount: u64) -> u64 {
    bps_of(amount, p.protocol_fee_bps).max(p.min_fee)
}

/// Pay the worker `amount - fee`, return its bond, record the outcome.
fn settle(c: &mut Config, j: &mut Job, worker: &mut Agent, outcome: Outcome, review: bool) {
    j.state = JobState::Settled;
    j.outcome = Some(outcome);
    c.totals.locked -= j.amount;
    release_bond(j, worker);
    let paid = j.amount - j.fee;
    credit(c, worker, paid);
    c.totals.fees_owed += j.fee;
    match outcome {
        Outcome::Unresolved => worker.worker_record.unresolved += 1,
        _ => worker.worker_record.completed += 1,
    }
    emit!(JobSettled { job_id: j.id, worker: worker.id, paid, fee: j.fee, outcome });
    if review {
        review_worker(j, worker, 100);
    }
}

/// Return `amount` to the client. Only a FAILED delivery costs the worker
/// its bond; every other refund releases it.
fn refund(c: &mut Config, j: &mut Job, client: &mut Agent, worker: &mut Agent, reason: &str, outcome: Option<Outcome>) {
    j.state = JobState::Refunded;
    j.outcome = outcome;
    c.totals.locked -= j.amount;
    credit(c, client, j.amount);
    match outcome {
        Some(Outcome::Failed) => {
            seize_bond(c, j, client, worker);
            worker.worker_record.failed += 1;
            review_worker(j, worker, 0);
        }
        Some(Outcome::DisputeLost) => {
            release_bond(j, worker);
            worker.worker_record.disputes_lost += 1;
            review_worker(j, worker, 0);
        }
        _ => release_bond(j, worker),
    }
    emit!(JobRefunded { job_id: j.id, client: client.id, amount: j.amount, reason: reason.into(), outcome });
}

fn release_bond(j: &mut Job, worker: &mut Agent) {
    if !j.bond_locked {
        return;
    }
    worker.stake.locked -= j.worker_bond;
    j.bond_locked = false;
}

fn seize_bond(c: &mut Config, j: &mut Job, client: &mut Agent, worker: &mut Agent) {
    if !j.bond_locked {
        return;
    }
    let bond = j.worker_bond;
    let take = bond.min(worker.stake.available);
    worker.stake.available -= take;
    worker.stake.locked -= bond;
    j.bond_locked = false;
    c.totals.stakes -= take;
    credit(c, client, take);
    emit!(BondSeized { job_id: j.id, worker: worker.id, client: client.id, amount: take });
}

fn review_worker(j: &Job, worker: &mut Agent, value: u8) {
    worker.worker_record.reviews += 1;
    worker.worker_record.review_score_sum += value as u64;
    emit!(Review { job_id: j.id, agent_id: worker.id, side: 0, value });
}

fn review_client(j: &Job, client: &mut Agent, value: u8) {
    client.client_record.reviews += 1;
    client.client_record.review_score_sum += value as u64;
    emit!(Review { job_id: j.id, agent_id: client.id, side: 1, value });
}

fn pay_in(ctx: &Context<Deposit>, amount: u64) -> Result<()> {
    let a = &ctx.accounts;
    token::transfer_checked(
        CpiContext::new(
            a.token_program.key(),
            TransferChecked {
                from: a.from.to_account_info(),
                mint: a.mint.to_account_info(),
                to: a.vault.to_account_info(),
                authority: a.payer.to_account_info(),
            },
        ),
        amount,
        a.mint.decimals,
    )
}

fn pay_out<'info>(
    token_program: &Program<'info, Token>,
    vault: &Account<'info, TokenAccount>,
    mint: &Account<'info, Mint>,
    to: &Account<'info, TokenAccount>,
    config: &Account<'info, Config>,
    amount: u64,
) -> Result<()> {
    let bump = [config.bump];
    let seeds: &[&[u8]] = &[CONFIG_SEED, &bump];
    let signer = &[seeds];
    token::transfer_checked(
        CpiContext::new_with_signer(
            token_program.key(),
            TransferChecked {
                from: vault.to_account_info(),
                mint: mint.to_account_info(),
                to: to.to_account_info(),
                authority: config.to_account_info(),
            },
            signer,
        ),
        amount,
        mint.decimals,
    )
}

fn check_params(p: &Params) -> Result<()> {
    require!(p.protocol_fee_bps <= MAX_FEE_BPS, FidoraError::FeeTooHigh);
    for w in [p.review_window, p.max_accept_window, p.max_work_window, p.dispute_timeout] {
        require!((1..=MAX_WINDOW).contains(&w), FidoraError::InvalidWindow);
    }
    require!((1..=MAX_WITHDRAW_DELAY).contains(&p.withdraw_delay), FidoraError::InvalidWindow);
    require!(p.min_job_amount > 0 && p.min_fee < p.min_job_amount, FidoraError::InvalidFeeFloor);
    require!(p.fast_path_max == 0 || p.fast_path_max >= p.min_job_amount, FidoraError::InvalidFeeFloor);
    require!(
        p.max_job_amount >= p.min_job_amount && p.max_job_amount >= p.fast_path_max,
        FidoraError::InvalidFeeFloor
    );
    require!(p.bond_bps as u128 <= BPS && p.dispute_bond_bps as u128 <= BPS, FidoraError::InvalidBond);
    require!(p.min_stake > 0, FidoraError::InvalidStake);
    require!(p.default_per_task_cap <= p.default_daily_cap, FidoraError::InvalidCaps);
    Ok(())
}
