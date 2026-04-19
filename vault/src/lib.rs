#![allow(unexpected_cfgs)]
#![allow(ambiguous_glob_reexports)]

use anchor_lang::prelude::*;

pub mod errors;
pub mod instructions;
pub mod state;

pub use errors::*;
pub use instructions::*;
pub use state::*;

declare_id!("A8gnNqXsVFon2h6wGwUWyBFMDY5nrjNJfc1FvW6k2LMm");

/// xVault — Token-2022 index-vault program.
///
/// ### Scaled UI invariant
/// All on-chain balances and instruction amounts are **raw** Token-2022 amounts.
/// Display = raw × UI multiplier (read off-chain from the mint's ScaledUiAmountConfig
/// extension). NAV math must scale raw × multiplier before pricing; see oracle program.
#[program]
pub mod xvault_vault {
    use super::*;

    /// Creates a vault SKU with fixed weights and 24h rebalance cadence.
    pub fn init_vault(ctx: Context<InitVault>, args: InitVaultArgs) -> Result<()> {
        instructions::init_vault::handler(ctx, args)
    }

    /// Deposits a single whitelisted basket asset (raw) in exchange for share tokens.
    /// NAV is read from the oracle sibling account; shares minted = raw_usd_value / nav_per_share.
    pub fn deposit(ctx: Context<Deposit>, raw_amount: u64, min_shares_out: u64) -> Result<()> {
        instructions::deposit::handler(ctx, raw_amount, min_shares_out)
    }

    /// Burns shares, returns pro-rata slice of each holding (raw amounts). No swap.
    pub fn withdraw_in_kind<'info>(
        ctx: Context<'_, '_, 'info, 'info, WithdrawInKind<'info>>,
        shares: u64,
    ) -> Result<()> {
        instructions::withdraw_in_kind::handler(ctx, shares)
    }

    /// Burns shares and returns USDC by routing through Jupiter.
    pub fn withdraw_usdc(
        ctx: Context<WithdrawUsdc>,
        shares: u64,
        max_slip_bps: u16,
    ) -> Result<()> {
        instructions::withdraw_usdc::handler(ctx, shares, max_slip_bps)
    }

    /// Executes one leg of a rebalance (swap A→B via Jupiter CPI).
    /// Caller is keeper; `require!` trading_hours_mode compatibility handled off-chain;
    /// on-chain enforces NAV-preservation slippage bound.
    pub fn rebalance_leg<'info>(
        ctx: Context<'_, '_, 'info, 'info, RebalanceLeg<'info>>,
        args: RebalanceLegArgs,
    ) -> Result<()> {
        instructions::rebalance_leg::handler(ctx, args)
    }

    /// Updates target weights. Gated by `vault.admin` (Squads multisig).
    pub fn update_weights(ctx: Context<UpdateWeights>, weights_bps: Vec<u16>) -> Result<()> {
        instructions::update_weights::handler(ctx, weights_bps)
    }

    /// Emergency pause — blocks deposits / rebalances; withdrawals remain open.
    pub fn set_paused(ctx: Context<SetPaused>, paused: bool) -> Result<()> {
        instructions::set_paused::handler(ctx, paused)
    }
}
