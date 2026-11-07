//! Events (Anchor `emit!`: base64 "Program data:" log lines, 8-byte
//! discriminator = sha256("event:<Name>")[..8]). One per state change the
//! indexer needs; the same set Pactora emits on NEAR.

use anchor_lang::prelude::*;

use crate::state::Outcome;

#[event]
pub struct AgentRegistered {
    pub agent_id: u64,
    pub owner: Pubkey,
    pub wallet: Pubkey,
    pub card_uri: String,
}

#[event]
pub struct WalletAccepted {
    pub agent_id: u64,
    pub wallet: Pubkey,
    pub previous: Option<Pubkey>,
}

#[event]
pub struct WalletProposed {
    pub agent_id: u64,
    pub wallet: Option<Pubkey>,
}

#[event]
pub struct AgentFrozen {
    pub agent_id: u64,
    pub frozen: bool,
}

#[event]
pub struct CapsSet {
    pub agent_id: u64,
    pub per_task_cap: u64,
    pub daily_cap: u64,
}

/// `kind` 0 = spendable balance, 1 = stake. `from` is the token owner that paid.
#[event]
pub struct Funded {
    pub agent_id: u64,
    pub from: Pubkey,
    pub amount: u64,
    pub kind: u8,
}

#[event]
pub struct Withdrawn {
    pub agent_id: u64,
    pub to: Pubkey,
    pub amount: u64,
}

#[event]
pub struct JobCreated {
    pub job_id: u64,
    pub client: u64,
    pub worker: u64,
    pub amount: u64,
    pub fee: u64,
    pub spec_hash: [u8; 32],
    pub accept_deadline: u64,
    pub work_deadline: u64,
}

#[event]
pub struct JobAccepted {
    pub job_id: u64,
    pub worker: u64,
    pub bond: u64,
    pub work_deadline: u64,
}

#[event]
pub struct ResultSubmitted {
    pub job_id: u64,
    pub result_hash: [u8; 32],
    pub review_deadline: u64,
}

#[event]
pub struct JobDisputed {
    pub job_id: u64,
    pub bond: u64,
    pub dispute_deadline: u64,
    pub reason_hash: Option<[u8; 32]>,
}

#[event]
pub struct DisputeResolved {
    pub job_id: u64,
    pub for_worker: bool,
    pub arbiter: Pubkey,
}

#[event]
pub struct DisputeExpired {
    pub job_id: u64,
}

#[event]
pub struct JobSettled {
    pub job_id: u64,
    pub worker: u64,
    pub paid: u64,
    pub fee: u64,
    pub outcome: Outcome,
}

/// `reason`: "cancelled", "unaccepted", "undelivered", "dispute".
#[event]
pub struct JobRefunded {
    pub job_id: u64,
    pub client: u64,
    pub amount: u64,
    pub reason: String,
    pub outcome: Option<Outcome>,
}

#[event]
pub struct BondSeized {
    pub job_id: u64,
    pub worker: u64,
    pub client: u64,
    pub amount: u64,
}

#[event]
pub struct DisputeBondSettled {
    pub job_id: u64,
    pub to: u64,
    pub amount: u64,
}

#[event]
pub struct DirectPaid {
    pub job_id: u64,
    pub client: u64,
    pub worker: u64,
    pub amount: u64,
    pub fee: u64,
    pub spec_hash: [u8; 32],
}

/// `side` 0 = worker, 1 = client; `value` 0..=100.
#[event]
pub struct Review {
    pub job_id: u64,
    pub agent_id: u64,
    pub side: u8,
    pub value: u8,
}

#[event]
pub struct UnstakeRequested {
    pub agent_id: u64,
    pub amount: u64,
    pub unlock_at: u64,
}

#[event]
pub struct UnstakeCancelled {
    pub agent_id: u64,
    pub amount: u64,
}

#[event]
pub struct Unstaked {
    pub agent_id: u64,
    pub amount: u64,
}

#[event]
pub struct FeesClaimed {
    pub to: Pubkey,
    pub amount: u64,
}

#[event]
pub struct PausedSet {
    pub by: Pubkey,
    pub paused: bool,
}

/// `role`: "owner", "arbiter", "pauser", "fee_recipient", "params".
#[event]
pub struct RoleSet {
    pub role: String,
    pub account: Pubkey,
}
