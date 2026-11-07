//! Refusals. The names are the ones Pactora uses on NEAR and AgentX v3 on
//! EVM wherever the rule is the same, so the backend maps a refusal to one
//! meaning on every chain.

use anchor_lang::prelude::*;

#[error_code]
pub enum FidoraError {
    NotOwner,
    NotPendingOwner,
    NotArbiter,
    NotPauser,
    Paused,
    WrongMint,
    WrongRecipient,

    NoSuchAgent,
    NotAgentOwner,
    NotAgentWallet,
    NotPendingWallet,
    WalletTaken,
    AgentFrozen,
    CardUriTooLong,

    ZeroAmount,
    AmountBelowMinimum,
    AmountAboveMaximum,
    AmountTooLargeForFastPath,
    SelfDealing,
    SameOwner,
    NotHireable,
    WindowTooLong,

    NoSuchJob,
    InvalidState,
    DeadlinePassed,
    DeadlineNotPassed,

    InsufficientBalance,
    PerTaskCapExceeded,
    DailyCapExceeded,

    InsufficientBond,
    WithdrawAlreadyPending,
    NothingPending,
    StillLocked,
    NothingOwed,

    FeeTooHigh,
    InvalidWindow,
    InvalidFeeFloor,
    InvalidBond,
    InvalidStake,
    InvalidCaps,
}
