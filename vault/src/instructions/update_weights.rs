use anchor_lang::prelude::*;

use crate::errors::VaultError;
use crate::state::{Vault, BPS_DENOMINATOR};

#[derive(Accounts)]
pub struct UpdateWeights<'info> {
    pub admin: Signer<'info>,

    #[account(
        mut,
        seeds = [b"vault", &vault.sku[..]],
        bump = vault.bump,
        has_one = admin @ VaultError::Unauthorized,
    )]
    pub vault: Account<'info, Vault>,
}

pub fn handler(ctx: Context<UpdateWeights>, weights_bps: Vec<u16>) -> Result<()> {
    let vault = &mut ctx.accounts.vault;
    require!(
        weights_bps.len() == vault.holdings.len(),
        VaultError::WeightsLengthMismatch
    );
    let sum: u32 = weights_bps.iter().map(|w| *w as u32).sum();
    require!(sum as u16 == BPS_DENOMINATOR, VaultError::WeightsSumInvalid);

    for (h, w) in vault.holdings.iter_mut().zip(weights_bps.iter()) {
        h.target_weight_bps = *w;
    }
    Ok(())
}
