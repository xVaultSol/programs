use anchor_lang::prelude::*;
use anchor_spl::token_interface::{mint_to, Mint, MintTo, TokenAccount, TokenInterface};
use xvault_oracle::NavSnapshot;

use crate::errors::VaultError;
use crate::state::{Vault, BPS_DENOM};

const MULTIPLIER_DENOM_1E18: u128 = 1_000_000_000_000_000_000u128;
const PRICE_SCALE_1E8: u128 = 100_000_000;
const USDC_DECIMALS: u32 = 6;

/// Permissionless — collects performance fee when NAV/share exceeds the high-water mark.
/// Mints fee shares to treasury = perf_fee_bps/BPS × (nav_per_share - hwm) × supply / nav_per_share.
#[derive(Accounts)]
pub struct CollectPerfFee<'info> {
    pub caller: Signer<'info>,

    #[account(
        mut,
        seeds = [b"vault", &vault.sku[..]],
        bump = vault.bump,
    )]
    pub vault: Account<'info, Vault>,

    #[account(address = vault.nav_snapshot)]
    pub nav_snapshot: Box<Account<'info, NavSnapshot>>,

    #[account(
        mut,
        address = vault.share_mint,
    )]
    pub share_mint: InterfaceAccount<'info, Mint>,

    #[account(
        mut,
        token::mint = share_mint,
        token::authority = treasury_authority,
        token::token_program = token_program,
    )]
    pub treasury_share_ata: InterfaceAccount<'info, TokenAccount>,

    /// CHECK: validated as vault.treasury
    #[account(address = vault.treasury)]
    pub treasury_authority: UncheckedAccount<'info>,

    pub token_program: Interface<'info, TokenInterface>,
}

pub fn handler(ctx: Context<CollectPerfFee>) -> Result<()> {
    let vault = &ctx.accounts.vault;
    let nav = &ctx.accounts.nav_snapshot;

    let now_slot = Clock::get()?.slot;
    require!(
        now_slot.saturating_sub(nav.last_update_slot) <= nav.max_stale_slots,
        VaultError::OracleStale
    );

    let total_supply = ctx.accounts.share_mint.supply;
    if total_supply == 0 {
        return Ok(());
    }

    // Compute total NAV in 1e8.
    let mut total_nav_1e8: u128 = 0;
    for h in vault.holdings.iter() {
        if let Some(e) = nav.entries.iter().find(|entry| entry.mint == h.mint) {
            let usd = usd_value_1e8(h.raw_balance, e.multiplier_num, e.price_usd_1e8, h.decimals)?;
            total_nav_1e8 = total_nav_1e8
                .checked_add(usd)
                .ok_or(VaultError::MathOverflow)?;
        }
    }
    // Include USDC cash buffer.
    let cash_usd_1e8 = (vault.cash_raw as u128)
        .checked_mul(PRICE_SCALE_1E8)
        .ok_or(VaultError::MathOverflow)?
        .checked_div(10u128.pow(USDC_DECIMALS))
        .ok_or(VaultError::MathOverflow)?;
    total_nav_1e8 = total_nav_1e8
        .checked_add(cash_usd_1e8)
        .ok_or(VaultError::MathOverflow)?;

    // NAV per share in 1e8.
    let nav_per_share_1e8 = total_nav_1e8
        .checked_mul(PRICE_SCALE_1E8)
        .ok_or(VaultError::MathOverflow)?
        .checked_div(total_supply as u128)
        .ok_or(VaultError::MathOverflow)?;

    let hwm = vault.hwm_nav_per_share_1e8;
    if hwm == 0 {
        // First time: just set the HWM, no fee to collect.
        ctx.accounts.vault.hwm_nav_per_share_1e8 = nav_per_share_1e8;
        return Ok(());
    }

    require!(nav_per_share_1e8 > hwm, VaultError::BelowHighWaterMark);

    let gain_1e8 = nav_per_share_1e8
        .checked_sub(hwm)
        .ok_or(VaultError::MathOverflow)?;

    // fee_shares = performance_fee_bps / BPS × gain / nav_per_share × total_supply
    let fee_shares = (vault.performance_fee_bps as u128)
        .checked_mul(gain_1e8)
        .ok_or(VaultError::MathOverflow)?
        .checked_mul(total_supply as u128)
        .ok_or(VaultError::MathOverflow)?
        .checked_div(
            BPS_DENOM
                .checked_mul(nav_per_share_1e8)
                .ok_or(VaultError::MathOverflow)?,
        )
        .ok_or(VaultError::MathOverflow)?;

    let fee_shares_u64 = u64::try_from(fee_shares).map_err(|_| VaultError::MathOverflow)?;

    if fee_shares_u64 > 0 {
        let sku_seed = &ctx.accounts.vault.sku[..];
        let bump_seed = [ctx.accounts.vault.bump];
        let signer_seeds: &[&[u8]] = &[b"vault", sku_seed, &bump_seed];
        let signer = [signer_seeds];

        let mint_ctx = CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            MintTo {
                mint: ctx.accounts.share_mint.to_account_info(),
                to: ctx.accounts.treasury_share_ata.to_account_info(),
                authority: ctx.accounts.vault.to_account_info(),
            },
            &signer,
        );
        mint_to(mint_ctx, fee_shares_u64)?;
    }

    // Update HWM.
    ctx.accounts.vault.hwm_nav_per_share_1e8 = nav_per_share_1e8;

    emit!(PerfFeeCollectedEvent {
        vault: ctx.accounts.vault.key(),
        fee_shares: fee_shares_u64,
        nav_per_share_1e8,
        previous_hwm_1e8: hwm,
    });

    Ok(())
}

#[event]
pub struct PerfFeeCollectedEvent {
    pub vault: Pubkey,
    pub fee_shares: u64,
    pub nav_per_share_1e8: u128,
    pub previous_hwm_1e8: u128,
}

fn usd_value_1e8(raw_amount: u64, multiplier_num: u128, price_usd_1e8: u64, decimals: u8) -> Result<u128> {
    let scaled_raw = (raw_amount as u128)
        .checked_mul(multiplier_num)
        .ok_or(VaultError::MathOverflow)?
        .checked_div(MULTIPLIER_DENOM_1E18)
        .ok_or(VaultError::MathOverflow)?;
    let divisor = 10u128
        .checked_pow(decimals as u32)
        .ok_or(VaultError::MathOverflow)?;
    scaled_raw
        .checked_mul(price_usd_1e8 as u128)
        .ok_or(VaultError::MathOverflow)?
        .checked_div(divisor)
        .ok_or(VaultError::MathOverflow.into())
}
