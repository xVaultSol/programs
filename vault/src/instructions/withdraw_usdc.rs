use anchor_lang::prelude::*;
use anchor_spl::token_interface::{
    burn, transfer_checked, Burn, Mint, TokenAccount, TokenInterface, TransferChecked,
};
use xvault_oracle::NavSnapshot;

use crate::errors::VaultError;
use crate::state::{Vault, WITHDRAW_FEE_BPS, BPS_DENOM};

const MULTIPLIER_DENOM_1E18: u128 = 1_000_000_000_000_000_000u128;
const PRICE_SCALE_1E8: u128 = 100_000_000u128;
const USDC_DECIMALS: u32 = 6;
const MAX_WITHDRAW_SLIPPAGE_BPS: u16 = 500;

#[derive(Accounts)]
pub struct WithdrawUsdc<'info> {
    #[account(mut)]
    pub user: Signer<'info>,

    #[account(
        mut,
        seeds = [b"vault", &vault.sku[..]],
        bump = vault.bump,
    )]
    pub vault: Account<'info, Vault>,

    #[account(mut, address = vault.share_mint)]
    pub share_mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(
        mut,
        token::mint = share_mint,
        token::authority = user,
        token::token_program = share_token_program,
    )]
    pub user_share_ata: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(address = vault.nav_snapshot)]
    pub nav_snapshot: Box<Account<'info, NavSnapshot>>,

    pub usdc_mint: InterfaceAccount<'info, Mint>,

    #[account(
        mut,
        token::mint = usdc_mint,
        token::authority = vault,
        token::token_program = stable_token_program,
    )]
    pub vault_usdc_ata: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(
        mut,
        token::mint = usdc_mint,
        token::authority = user,
        token::token_program = stable_token_program,
    )]
    pub user_usdc_ata: InterfaceAccount<'info, TokenAccount>,

    /// Token program that owns the USDC/stablecoin mint (classic SPL on mainnet).
    pub stable_token_program: Interface<'info, TokenInterface>,
    /// Token program that owns the share mint (Token-2022).
    pub share_token_program: Interface<'info, TokenInterface>,
}

pub fn handler(ctx: Context<WithdrawUsdc>, shares: u64, max_slip_bps: u16) -> Result<()> {
    require!(shares > 0, VaultError::ZeroAmount);
    require!(max_slip_bps <= MAX_WITHDRAW_SLIPPAGE_BPS, VaultError::SlippageExceeded);

    let total_shares = ctx.accounts.share_mint.supply;
    require!(total_shares > 0, VaultError::MathOverflow);

    let burn_ctx = CpiContext::new(
        ctx.accounts.share_token_program.to_account_info(),
        Burn {
            mint: ctx.accounts.share_mint.to_account_info(),
            from: ctx.accounts.user_share_ata.to_account_info(),
            authority: ctx.accounts.user.to_account_info(),
        },
    );
    burn(burn_ctx, shares)?;

    let nav = &ctx.accounts.nav_snapshot;
    let now_slot = Clock::get()?.slot;
    require!(
        now_slot.saturating_sub(nav.last_update_slot) <= nav.max_stale_slots,
        VaultError::OracleStale
    );

    let mut total_nav_1e8: u128 = 0;
    for h in ctx.accounts.vault.holdings.iter() {
        if let Some(e) = nav.entries.iter().find(|entry| entry.mint == h.mint) {
            let usd = usd_value_1e8(h.raw_balance, e.multiplier_num, e.price_usd_1e8, h.decimals)?;
            total_nav_1e8 = total_nav_1e8.checked_add(usd).ok_or(VaultError::MathOverflow)?;
        }
    }

    // Include USDC cash buffer in NAV.
    let cash_usd_1e8 = (ctx.accounts.vault.cash_raw as u128)
        .checked_mul(PRICE_SCALE_1E8)
        .ok_or(VaultError::MathOverflow)?
        .checked_div(10u128.pow(USDC_DECIMALS))
        .ok_or(VaultError::MathOverflow)?;
    total_nav_1e8 = total_nav_1e8
        .checked_add(cash_usd_1e8)
        .ok_or(VaultError::MathOverflow)?;

    let expected_usdc_raw = total_nav_1e8
        .checked_mul(shares as u128)
        .ok_or(VaultError::MathOverflow)?
        .checked_div(total_shares as u128)
        .ok_or(VaultError::MathOverflow)?
        .checked_mul(10u128.pow(USDC_DECIMALS))
        .ok_or(VaultError::MathOverflow)?
        .checked_div(PRICE_SCALE_1E8)
        .ok_or(VaultError::MathOverflow)?;

    let available_usdc_raw = expected_usdc_raw
        .min(ctx.accounts.vault.cash_raw as u128)
        .min(ctx.accounts.vault_usdc_ata.amount as u128);
    let min_out = expected_usdc_raw
        .checked_mul(BPS_DENOM.saturating_sub(max_slip_bps as u128))
        .ok_or(VaultError::MathOverflow)?
        .checked_div(BPS_DENOM)
        .ok_or(VaultError::MathOverflow)?;
    require!(available_usdc_raw >= min_out, VaultError::SlippageExceeded);

    // Deduct 0.05% protocol withdrawal fee.
    let fee_raw = available_usdc_raw
        .checked_mul(WITHDRAW_FEE_BPS)
        .ok_or(VaultError::MathOverflow)?
        .checked_div(BPS_DENOM)
        .ok_or(VaultError::MathOverflow)?;
    let user_out = available_usdc_raw
        .checked_sub(fee_raw)
        .ok_or(VaultError::MathOverflow)?;
    let user_out_u64 = u64::try_from(user_out).map_err(|_| VaultError::MathOverflow)?;
    let fee_u64 = u64::try_from(fee_raw).map_err(|_| VaultError::MathOverflow)?;
    let total_deducted = user_out_u64
        .checked_add(fee_u64)
        .ok_or(VaultError::MathOverflow)?;

    let sku_seed = &ctx.accounts.vault.sku[..];
    let bump_seed = [ctx.accounts.vault.bump];
    let signer_seeds: &[&[u8]] = &[b"vault", sku_seed, &bump_seed];
    let signer = [signer_seeds];
    let transfer_ctx = CpiContext::new_with_signer(
        ctx.accounts.stable_token_program.to_account_info(),
        TransferChecked {
            from: ctx.accounts.vault_usdc_ata.to_account_info(),
            mint: ctx.accounts.usdc_mint.to_account_info(),
            to: ctx.accounts.user_usdc_ata.to_account_info(),
            authority: ctx.accounts.vault.to_account_info(),
        },
        &signer,
    );
    transfer_checked(transfer_ctx, user_out_u64, ctx.accounts.usdc_mint.decimals)?;

    let vault = &mut ctx.accounts.vault;
    vault.accrued_protocol_fees_raw = vault
        .accrued_protocol_fees_raw
        .checked_add(fee_u64)
        .ok_or(VaultError::MathOverflow)?;
    vault.cash_raw = vault
        .cash_raw
        .checked_sub(total_deducted)
        .ok_or(VaultError::MathOverflow)?;

    emit!(WithdrawUsdcEvent {
        user: ctx.accounts.user.key(),
        shares_burned: shares,
        usdc_raw_out: user_out_u64,
        fee_raw: fee_u64,
        max_slip_bps,
    });

    Ok(())
}

#[event]
pub struct WithdrawUsdcEvent {
    pub user: Pubkey,
    pub shares_burned: u64,
    pub usdc_raw_out: u64,
    pub fee_raw: u64,
    pub max_slip_bps: u16,
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
