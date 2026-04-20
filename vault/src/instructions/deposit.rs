use anchor_lang::prelude::*;
use anchor_spl::token_interface::{
    mint_to, transfer_checked, Mint, MintTo, TokenAccount, TokenInterface, TransferChecked,
};
use xvault_oracle::NavSnapshot;

use crate::errors::VaultError;
use crate::state::Vault;

const MULTIPLIER_DENOM_1E18: u128 = 1_000_000_000_000_000_000u128;
const PRICE_SCALE_1E8: u128 = 100_000_000u128;
const USDC_DECIMALS: u32 = 6;

#[derive(Accounts)]
pub struct Deposit<'info> {
    #[account(mut)]
    pub user: Signer<'info>,

    #[account(
        mut,
        seeds = [b"vault", &vault.sku[..]],
        bump = vault.bump,
    )]
    pub vault: Account<'info, Vault>,

    /// USDC mint (standard: EPjFWdd5auEH7vH2pv6HYzpg8aGDkxwTLjZKLRPH9ss)
    pub usdc_mint: InterfaceAccount<'info, Mint>,

    #[account(
        mut,
        token::mint = usdc_mint,
        token::authority = user,
        token::token_program = stable_token_program,
    )]
    pub user_usdc_ata: InterfaceAccount<'info, TokenAccount>,

    #[account(
        mut,
        token::mint = usdc_mint,
        token::authority = vault,
        token::token_program = stable_token_program,
    )]
    pub vault_usdc_ata: InterfaceAccount<'info, TokenAccount>,

    #[account(address = vault.nav_snapshot)]
    pub nav_snapshot: Box<Account<'info, NavSnapshot>>,

    #[account(
        mut,
        address = vault.share_mint,
    )]
    pub share_mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(
        mut,
        token::mint = share_mint,
        token::authority = user,
        token::token_program = share_token_program,
    )]
    pub user_share_ata: Box<InterfaceAccount<'info, TokenAccount>>,

    /// Token program that owns the USDC/stablecoin mint (classic SPL on mainnet).
    pub stable_token_program: Interface<'info, TokenInterface>,
    /// Token program that owns the share mint (Token-2022).
    pub share_token_program: Interface<'info, TokenInterface>,
}

pub fn handler(ctx: Context<Deposit>, usdc_raw: u64, min_shares_out: u64) -> Result<()> {
    require!(!ctx.accounts.vault.paused, VaultError::Paused);
    require!(usdc_raw > 0, VaultError::ZeroAmount);

    // Pull USDC from user → vault cash buffer via the stable token program.
    let cpi_ctx = CpiContext::new(
        ctx.accounts.stable_token_program.to_account_info(),
        TransferChecked {
            from: ctx.accounts.user_usdc_ata.to_account_info(),
            mint: ctx.accounts.usdc_mint.to_account_info(),
            to: ctx.accounts.vault_usdc_ata.to_account_info(),
            authority: ctx.accounts.user.to_account_info(),
        },
    );
    transfer_checked(cpi_ctx, usdc_raw, ctx.accounts.usdc_mint.decimals)?;

    let now_slot = Clock::get()?.slot;
    let nav = &ctx.accounts.nav_snapshot;
    require!(
        now_slot.saturating_sub(nav.last_update_slot) <= nav.max_stale_slots,
        VaultError::OracleStale
    );

    // Compute total NAV in 1e8 from snapshot entries and vault raw balances.
    let mut total_nav_1e8: u128 = 0;
    for h in ctx.accounts.vault.holdings.iter() {
        if let Some(e) = nav.entries.iter().find(|entry| entry.mint == h.mint) {
            let usd = usd_value_1e8(h.raw_balance, e.multiplier_num, e.price_usd_1e8, h.decimals)?;
            total_nav_1e8 = total_nav_1e8
                .checked_add(usd)
                .ok_or(VaultError::MathOverflow)?;
        }
    }

    // Include existing cash buffer in NAV.
    let cash_usd_1e8 = (ctx.accounts.vault.cash_raw as u128)
        .checked_mul(PRICE_SCALE_1E8)
        .ok_or(VaultError::MathOverflow)?
        .checked_div(10u128.pow(USDC_DECIMALS))
        .ok_or(VaultError::MathOverflow)?;
    total_nav_1e8 = total_nav_1e8
        .checked_add(cash_usd_1e8)
        .ok_or(VaultError::MathOverflow)?;

    // Convert USDC deposit to USD value (1e8).
    let deposit_usd_1e8 = (usdc_raw as u128)
        .checked_mul(PRICE_SCALE_1E8)
        .ok_or(VaultError::MathOverflow)?
        .checked_div(10u128.pow(USDC_DECIMALS))
        .ok_or(VaultError::MathOverflow)?;

    let share_supply = ctx.accounts.share_mint.supply as u128;
    let shares_out_u128 = shares_out_from_nav(deposit_usd_1e8, share_supply, total_nav_1e8)?;
    let shares_out = u64::try_from(shares_out_u128).map_err(|_| VaultError::MathOverflow)?;
    require!(shares_out >= min_shares_out, VaultError::SlippageExceeded);

    // Capture vault state before mutation (borrow checker).
    let sku_seed_copy = ctx.accounts.vault.sku.clone();
    let bump_seed_copy = ctx.accounts.vault.bump;

    {
        let vault = &mut ctx.accounts.vault;
        vault.cash_raw = vault
            .cash_raw
            .checked_add(usdc_raw)
            .ok_or(VaultError::MathOverflow)?;
    } // Release the mutable borrow

    let cash_buffer_after = ctx.accounts.vault.cash_raw;

    // Mint shares using NAV-based output.
    let sku_seed = &sku_seed_copy[..];
    let bump_seed = [bump_seed_copy];
    let signer_seeds: &[&[u8]] = &[b"vault", sku_seed, &bump_seed];
    let signer = [signer_seeds];
    
    let mint_ctx = CpiContext::new_with_signer(
        ctx.accounts.share_token_program.to_account_info(),
        MintTo {
            mint: ctx.accounts.share_mint.to_account_info(),
            to: ctx.accounts.user_share_ata.to_account_info(),
            authority: ctx.accounts.vault.to_account_info(),
        },
        &signer,
    );
    mint_to(mint_ctx, shares_out)?;

    emit!(DepositEvent {
        user: ctx.accounts.user.key(),
        usdc_raw,
        shares_minted: shares_out,
        cash_buffer_after,
    });

    Ok(())
}

#[event]
pub struct DepositEvent {
    pub user: Pubkey,
    pub usdc_raw: u64,
    pub shares_minted: u64,
    pub cash_buffer_after: u64,
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

fn shares_out_from_nav(deposit_usd_1e8: u128, share_supply: u128, total_nav_1e8: u128) -> Result<u128> {
    if share_supply == 0 || total_nav_1e8 == 0 {
        // Bootstrap: 1 share == $1 (1e8 scaling).
        return Ok(deposit_usd_1e8);
    }

    deposit_usd_1e8
        .checked_mul(share_supply)
        .ok_or(VaultError::MathOverflow)?
        .checked_div(total_nav_1e8)
        .ok_or(VaultError::MathOverflow.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mul_num(multiplier: f64) -> u128 {
        (multiplier * 1_000_000_000_000_000_000f64) as u128
    }

    #[test]
    fn usd_value_respects_scaled_ui_multiplier_set() {
        let raw_amount = 2_000_000u64; // 2 tokens @ 6 decimals
        let price_1e8 = 125_000_000u64; // $1.25
        let decimals = 6u8;

        let half = usd_value_1e8(raw_amount, mul_num(0.5), price_1e8, decimals).unwrap();
        let one = usd_value_1e8(raw_amount, mul_num(1.0), price_1e8, decimals).unwrap();
        let one_seventy_three =
            usd_value_1e8(raw_amount, mul_num(1.73), price_1e8, decimals).unwrap();

        assert_eq!(half, 125_000_000u128);
        assert_eq!(one, 250_000_000u128);
        assert_eq!(one_seventy_three, 432_500_000u128);
    }

    #[test]
    fn shares_out_tracks_multiplier_change() {
        let raw_amount = 1_000_000u64; // 1 token @ 6 decimals
        let price_1e8 = 200_000_000u64; // $2.00
        let decimals = 6u8;
        let share_supply = 1_000_000_000u128;
        let total_nav = 10_000_000_000u128; // $100 nav

        let out_half = shares_out_from_nav(
            usd_value_1e8(raw_amount, mul_num(0.5), price_1e8, decimals).unwrap(),
            share_supply,
            total_nav,
        )
        .unwrap();
        let out_one = shares_out_from_nav(
            usd_value_1e8(raw_amount, mul_num(1.0), price_1e8, decimals).unwrap(),
            share_supply,
            total_nav,
        )
        .unwrap();
        let out_one_seventy_three = shares_out_from_nav(
            usd_value_1e8(raw_amount, mul_num(1.73), price_1e8, decimals).unwrap(),
            share_supply,
            total_nav,
        )
        .unwrap();

        assert_eq!(out_half, 10_000_000u128);
        assert_eq!(out_one, 20_000_000u128);
        assert_eq!(out_one_seventy_three, 34_600_000u128);
    }

    #[test]
    fn multiplier_increase_mints_proportionally_more_shares() {
        let raw_amount = 3_000_000u64; // 3 tokens @ 6 decimals
        let price_1e8 = 150_000_000u64; // $1.50
        let decimals = 6u8;
        let share_supply = 2_000_000_000u128;
        let total_nav = 20_000_000_000u128;

        let out_one = shares_out_from_nav(
            usd_value_1e8(raw_amount, mul_num(1.0), price_1e8, decimals).unwrap(),
            share_supply,
            total_nav,
        )
        .unwrap();
        let out_one_seventy_three = shares_out_from_nav(
            usd_value_1e8(raw_amount, mul_num(1.73), price_1e8, decimals).unwrap(),
            share_supply,
            total_nav,
        )
        .unwrap();

        // 1.73x multiplier should mint exactly 1.73x shares under unchanged price/nav.
        assert_eq!(out_one, 45_000_000u128);
        assert_eq!(out_one_seventy_three, 77_850_000u128);
        assert_eq!(out_one_seventy_three, out_one * 173 / 100);
    }
}
