use anchor_lang::prelude::*;

use crate::errors::VaultError;
use crate::state::{PauseFlags, Vault};

#[derive(Accounts)]
pub struct SetPaused<'info> {
    pub admin: Signer<'info>,

    #[account(
        mut,
        seeds = [b"vault", &vault.sku[..]],
        bump = vault.bump,
        has_one = admin @ VaultError::Unauthorized,
    )]
    pub vault: Account<'info, Vault>,
}

/// Legacy emergency circuit breaker.
///
/// `paused == true` sets `halted`, blocking deposits / rebalances / fee collection.
/// `paused == false` clears `halted` only; callers must use `set_pause_flags` to clear
/// the finer-grained flags if they were set explicitly.
pub fn handler(ctx: Context<SetPaused>, paused: bool) -> Result<()> {
    ctx.accounts.vault.pause_flags.halted = paused;
    Ok(())
}

/// Granular pause control. Admin overwrites the full flag set.
pub fn set_pause_flags_handler(ctx: Context<SetPaused>, flags: PauseFlags) -> Result<()> {
    ctx.accounts.vault.pause_flags = flags;
    Ok(())
}
