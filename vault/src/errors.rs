use anchor_lang::prelude::*;

#[error_code]
pub enum VaultError {
    #[msg("Vault is paused")]
    Paused,
    #[msg("Unauthorized signer")]
    Unauthorized,
    #[msg("Weights must sum to 10_000 bps")]
    WeightsSumInvalid,
    #[msg("Weights length must match asset count")]
    WeightsLengthMismatch,
    #[msg("Asset not whitelisted for this vault")]
    AssetNotWhitelisted,
    #[msg("Slippage guard: min_shares_out not met")]
    SlippageExceeded,
    #[msg("Raw amount must be > 0")]
    ZeroAmount,
    #[msg("NAV oracle entry stale")]
    OracleStale,
    #[msg("Token-2022 multiplier on mint differs from oracle snapshot")]
    MultiplierMismatch,
    #[msg("Rebalance would violate NAV-preservation bound")]
    NavDriftExceeded,
    #[msg("Rebalance settlement validation failed")]
    RebalanceSettlementInvalid,
    #[msg("Basket must contain between 1 and 20 assets")]
    BasketSizeInvalid,
    #[msg("Invalid remaining accounts for token transfer legs")]
    InvalidRemainingAccounts,
    #[msg("Math overflow")]
    MathOverflow,
}
