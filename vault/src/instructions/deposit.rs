use anchor_lang::prelude::*;
use anchor_spl::token_interface::{
    mint_to, transfer_checked, Mint, MintTo, TokenAccount, TokenInterface, TransferChecked,
};
use xvault_oracle::NavSnapshot;

use crate::errors::VaultError;
use crate::state::Vault;

const MULTIPLIER_DENOM_1E18: u128 = 1_000_000_000_000_000_000u128;

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

    /// xStock Token-2022 mint the user is depositing. Must be whitelisted.
    pub asset_mint: InterfaceAccount<'info, Mint>,

    #[account(
        mut,
        token::mint = asset_mint,
        token::authority = user,
        token::token_program = token_program,
    )]
    pub user_ata: InterfaceAccount<'info, TokenAccount>,

    #[account(
        mut,
        seeds = [b"holding", &vault.sku[..], asset_mint.key().as_ref()],
        bump,
        token::mint = asset_mint,
        token::token_program = token_program,
    )]
    pub vault_holding_ata: InterfaceAccount<'info, TokenAccount>,

    #[account(address = vault.nav_snapshot)]
    pub nav_snapshot: Account<'info, NavSnapshot>,

    #[account(
        mut,
        address = vault.share_mint,
    )]
    pub share_mint: InterfaceAccount<'info, Mint>,

    #[account(
        mut,
        token::mint = share_mint,
        token::authority = user,
        token::token_program = token_program,
    )]
    pub user_share_ata: InterfaceAccount<'info, TokenAccount>,

    pub token_program: Interface<'info, TokenInterface>,
}

pub fn handler(ctx: Context<Deposit>, raw_amount: u64, min_shares_out: u64) -> Result<()> {
    require!(!ctx.accounts.vault.paused, VaultError::Paused);
    require!(raw_amount > 0, VaultError::ZeroAmount);

    let mint_key = ctx.accounts.asset_mint.key();
    let holding = ctx
        .accounts
        .vault
        .holding(&mint_key)
        .ok_or(VaultError::AssetNotWhitelisted)?;
    let decimals = holding.decimals;

    // Pull raw tokens from user → vault holding ATA (Token-2022 `transfer_checked`
    // validates decimals, avoiding silent scaled-amount mistakes).
    let cpi_ctx = CpiContext::new(
        ctx.accounts.token_program.to_account_info(),
        TransferChecked {
            from: ctx.accounts.user_ata.to_account_info(),
            mint: ctx.accounts.asset_mint.to_account_info(),
            to: ctx.accounts.vault_holding_ata.to_account_info(),
            authority: ctx.accounts.user.to_account_info(),
        },
    );
    transfer_checked(cpi_ctx, raw_amount, decimals)?;

    let now_slot = Clock::get()?.slot;
    let nav = &ctx.accounts.nav_snapshot;
    require!(
        now_slot.saturating_sub(nav.last_update_slot) <= nav.max_stale_slots,
        VaultError::OracleStale
    );

    let nav_entry = nav
        .entries
        .iter()
        .find(|e| e.mint == mint_key)
        .ok_or(VaultError::OracleStale)?;

    // Deposit leg USD value in fixed-point 1e8.
    let deposit_usd_1e8 = usd_value_1e8(
        raw_amount,
        nav_entry.multiplier_num,
        nav_entry.price_usd_1e8,
        decimals,
    )?;

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

    let share_supply = ctx.accounts.share_mint.supply as u128;
    let shares_out_u128 = shares_out_from_nav(deposit_usd_1e8, share_supply, total_nav_1e8)?;
    let shares_out = u64::try_from(shares_out_u128).map_err(|_| VaultError::MathOverflow)?;
    require!(shares_out >= min_shares_out, VaultError::SlippageExceeded);

    // Update raw balance.
    let vault = &mut ctx.accounts.vault;
    let holding_mut = vault
        .holding_mut(&mint_key)
        .ok_or(VaultError::AssetNotWhitelisted)?;
    holding_mut.raw_balance = holding_mut
        .raw_balance
        .checked_add(raw_amount)
        .ok_or(VaultError::MathOverflow)?;

    // Mint shares using NAV-based output.
    let sku_seed = &ctx.accounts.vault.sku[..];
    let bump_seed = [ctx.accounts.vault.bump];
    let signer_seeds: &[&[u8]] = &[b"vault", sku_seed, &bump_seed];
    let signer = [signer_seeds];
    let mint_ctx = CpiContext::new_with_signer(
        ctx.accounts.token_program.to_account_info(),
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
        asset: mint_key,
        raw_amount,
        shares_minted: shares_out,
    });

    Ok(())
}

#[event]
pub struct DepositEvent {
    pub user: Pubkey,
    pub asset: Pubkey,
    pub raw_amount: u64,
    pub shares_minted: u64,
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
