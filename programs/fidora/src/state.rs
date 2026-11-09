//! Stored types. Amounts are u64 base units of the SPL token (6 decimals).
//! Times are unix seconds from the Clock sysvar.

use anchor_lang::prelude::*;

/// Hourly buckets in the rolling 24 h window. 25, not 24, so a spend counts
/// for between 24 and 25 hours and never for less (same as the earlier editions).
pub const BUCKETS: u64 = 25;
pub const HOUR: u64 = 3_600;
/// Longest any window may be: one year.
pub const MAX_WINDOW: u64 = 365 * 24 * HOUR;
/// Longest a stake withdrawal may be delayed: 90 days.
pub const MAX_WITHDRAW_DELAY: u64 = 90 * 24 * HOUR;
pub const MAX_FEE_BPS: u16 = 1_000;
pub const BPS: u128 = 10_000;
pub const MAX_CARD_URI: usize = 200;

pub const CONFIG_SEED: &[u8] = b"config";
pub const VAULT_SEED: &[u8] = b"vault";
pub const AGENT_SEED: &[u8] = b"agent";
pub const JOB_SEED: &[u8] = b"job";
pub const WALLET_SEED: &[u8] = b"wallet";

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, PartialEq, InitSpace)]
pub struct Params {
    pub protocol_fee_bps: u16,
    pub min_fee: u64,
    pub min_job_amount: u64,
    pub max_job_amount: u64,
    /// Largest `direct_pay`; 0 turns the fast path off.
    pub fast_path_max: u64,
    pub review_window: u64,
    pub max_accept_window: u64,
    pub max_work_window: u64,
    pub dispute_timeout: u64,
    pub bond_bps: u16,
    pub min_bond: u64,
    pub dispute_bond_bps: u16,
    pub min_dispute_bond: u64,
    pub min_stake: u64,
    pub withdraw_delay: u64,
    pub default_per_task_cap: u64,
    pub default_daily_cap: u64,
}

/// Every token the vault holds is in exactly one of these buckets.
/// Invariant I1: vault balance == sum of all fields. (No `in_flight`: a
/// Solana transfer is atomic with the instruction that makes it.)
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, Default, PartialEq, InitSpace)]
pub struct Totals {
    pub locked: u64,
    pub dispute_bonds: u64,
    pub balances: u64,
    pub stakes: u64,
    pub fees_owed: u64,
}

impl Totals {
    pub fn sum(&self) -> u64 {
        self.locked + self.dispute_bonds + self.balances + self.stakes + self.fees_owed
    }
}

#[account]
#[derive(InitSpace)]
pub struct Config {
    pub owner: Pubkey,
    pub pending_owner: Option<Pubkey>,
    pub arbiter: Pubkey,
    pub pauser: Pubkey,
    pub fee_recipient: Pubkey,
    pub mint: Pubkey,
    pub paused: bool,
    pub params: Params,
    pub next_agent_id: u64,
    pub next_job_id: u64,
    pub totals: Totals,
    pub bump: u8,
    pub vault_bump: u8,
}

/// What the agent's wallet may spend, and what it has.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, PartialEq, InitSpace)]
pub struct Purse {
    pub balance: u64,
    pub per_task_cap: u64,
    pub daily_cap: u64,
    pub window_total: u64,
    pub last_hour: u64,
    pub buckets: [u64; 25],
}

impl Purse {
    pub fn new(per_task_cap: u64, daily_cap: u64) -> Self {
        Self {
            balance: 0,
            per_task_cap,
            daily_cap,
            window_total: 0,
            last_hour: 0,
            buckets: [0; 25],
        }
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, Default, PartialEq, InitSpace)]
pub struct Stake {
    /// Stake that counts, including what is locked against open jobs.
    pub available: u64,
    /// Requested for withdrawal; does not count towards hireability.
    pub pending: u64,
    pub unlock_at: u64,
    /// Sum of the bonds of this agent's accepted, unfinished jobs.
    pub locked: u64,
}

impl Stake {
    pub fn unlocked(&self) -> u64 {
        self.available.saturating_sub(self.locked)
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, Default, PartialEq, InitSpace)]
pub struct WorkerRecord {
    pub completed: u64,
    pub failed: u64,
    pub disputes_lost: u64,
    pub unresolved: u64,
    pub reviews: u64,
    pub review_score_sum: u64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, Default, PartialEq, InitSpace)]
pub struct ClientRecord {
    pub paid: u64,
    pub disputes_rejected: u64,
    pub disputes_upheld: u64,
    pub silent: u64,
    pub reviews: u64,
    pub review_score_sum: u64,
}

/// An agent: `owner` is the person (their wallet signs registration, caps
/// and every withdrawal); `wallet` is the keypair the agent signs jobs with.
/// The program accepts the wallet for job methods only, so a stolen agent
/// key can spend at most its caps and can never withdraw.
#[account]
#[derive(InitSpace)]
pub struct Agent {
    pub id: u64,
    pub owner: Pubkey,
    pub wallet: Option<Pubkey>,
    pub pending_wallet: Option<Pubkey>,
    #[max_len(200)]
    pub card_uri: String,
    pub frozen: bool,
    pub registered_at: u64,
    pub purse: Purse,
    pub stake: Stake,
    pub worker_record: WorkerRecord,
    pub client_record: ClientRecord,
    pub bump: u8,
}

/// One wallet acts for at most one agent. `agent_id` 0 means free (ids start at 1).
#[account]
#[derive(InitSpace)]
pub struct WalletIndex {
    pub agent_id: u64,
    pub bump: u8,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq, InitSpace)]
pub enum JobState {
    Created,
    Accepted,
    Submitted,
    Disputed,
    Settled,
    Refunded,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq, InitSpace)]
pub enum Outcome {
    Success,
    Failed,
    DisputeLost,
    Unresolved,
}

#[account]
#[derive(InitSpace)]
pub struct Job {
    pub id: u64,
    pub client: u64,
    pub worker: u64,
    pub amount: u64,
    pub fee: u64,
    pub state: JobState,
    pub accept_deadline: u64,
    pub work_deadline: u64,
    pub review_deadline: u64,
    pub dispute_deadline: u64,
    pub review_window: u64,
    pub dispute_timeout: u64,
    pub spec_hash: [u8; 32],
    pub result_hash: Option<[u8; 32]>,
    pub worker_bond: u64,
    pub dispute_bond: u64,
    /// True while `worker_bond` is locked in the worker's stake.
    pub bond_locked: bool,
    /// Hash of the reviewer's verdict (a judge's response) or the dispute reason.
    pub review_hash: Option<[u8; 32]>,
    pub outcome: Option<Outcome>,
    pub created_at: u64,
    pub bump: u8,
}
