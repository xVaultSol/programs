use anchor_lang::prelude::*;
use anchor_spl::token_interface::{Mint, TokenInterface};

use crate::errors::VaultError;
use crate::state::{Holding, Vault, BPS_DENOMINATOR, MAX_ASSETS};

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct InitVaultArgs {
    pub sku: [u8; 16],
    pub mints: Vec<Pubkey>,
    pub weights_bps: Vec<u16>,
    pub decimals: Vec<u8>,
    pub management_fee_bps: u16,
    pub performance_fee_bps: u16,
    pub rebalance_slippage_bps: u16,
    pub keeper: Pubkey,
    pub nav_snapshot: Pubkey,
    pub treasury: Pubkey,
}

#[derive(Accounts)]
#[instruction(args: InitVaultArgs)]
pub struct InitVault<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,

    #[account(
        init,
        payer = admin,
        space = Vault::size(args.mints.len()),
        seeds = [b"vault", &args.sku[..]],
        bump,
    )]
    pub vault: Account<'info, Vault>,

    /// Share mint (Token-2022). Initialized via separate ix (CPI) after this to keep size sane.
    /// We record its pubkey here.
    /// CHECK: validated via PDA seeds at `deposit` time.
    #[account(
        seeds = [b"share", &args.sku[..]],
        bump,
    )]
    pub share_mint: UncheckedAccount<'info>,

    pub token_program: Interface<'info, TokenInterface>,
    pub system_program: Program<'info, System>,
}

pub fn handler(ctx: Context<InitVault>, args: InitVaultArgs) -> Result<()> {
    require!(
        !args.mints.is_empty() && args.mints.len() <= MAX_ASSETS,
        VaultError::BasketSizeInvalid
    );
    require!(
        args.mints.len() == args.weights_bps.len() && args.mints.len() == args.decimals.len(),
        VaultError::WeightsLengthMismatch
    );
    let sum: u32 = args.weights_bps.iter().map(|w| *w as u32).sum();
    require!(sum as u16 == BPS_DENOMINATOR, VaultError::WeightsSumInvalid);

    let vault = &mut ctx.accounts.vault;
    vault.admin = ctx.accounts.admin.key();
    vault.keeper = args.keeper;
    vault.nav_snapshot = args.nav_snapshot;
    vault.share_mint = ctx.accounts.share_mint.key();
    vault.treasury = args.treasury;
    vault.sku = args.sku;
    vault.holdings = args
        .mints
        .iter()
        .zip(args.weights_bps.iter())
        .zip(args.decimals.iter())
        .map(|((mint, w), d)| Holding {
            mint: *mint,
            target_weight_bps: *w,
            raw_balance: 0,
            decimals: *d,
            _padding: [0; 5],
        })
        .collect();
    vault.cash_raw = 0;
    vault.management_fee_bps = args.management_fee_bps;
    vault.performance_fee_bps = args.performance_fee_bps;
    vault.rebalance_slippage_bps = args.rebalance_slippage_bps;
    vault.last_fee_collection_ts = Clock::get()?.unix_timestamp;
    vault.hwm_nav_per_share_1e8 = 0;
    vault.accrued_protocol_fees_raw = 0;
    vault.pause_flags = crate::state::PauseFlags::default();
    vault.bump = ctx.bumps.vault;
    vault.share_mint_bump = ctx.bumps.share_mint;
    vault._padding = [0; 2];

    // Silence unused
    let _ = ctx.accounts.token_program.key();
    let _: &Interface<TokenInterface> = &ctx.accounts.token_program;
    let _phantom: Option<InterfaceAccount<Mint>> = None;

    Ok(())
}
