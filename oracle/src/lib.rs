#![allow(unexpected_cfgs)]

use anchor_lang::prelude::*;

declare_id!("2VsYpWPX86ZMF2BCWZ9X6EiKe33ooHGLP5yPE4r1UnSU");

/// xVault oracle — stores keeper-pushed NAV snapshots (price × multiplier) per asset.
///
/// Why a program instead of reading Pyth directly: xStocks UI amount = raw × Token-2022
/// multiplier, and the multiplier can change intra-day (dividends, splits). The keeper
/// fetches both the multiplier (off-chain via xStocks `/v2/multiplier/{asset}`) and the
/// price (Pyth + xStocks `/v2/price-data/{asset}` cross-check), then writes a signed
/// snapshot used by the vault for deposit/withdraw share math.
#[program]
pub mod xvault_oracle {
    use super::*;

    pub fn init_snapshot(ctx: Context<InitSnapshot>, sku: [u8; 16]) -> Result<()> {
        let s = &mut ctx.accounts.snapshot;
        s.sku = sku;
        s.authority = ctx.accounts.authority.key();
        s.authorized_keepers = vec![ctx.accounts.authority.key()];
        s.max_stale_slots = 150;
        s.entries = Vec::new();
        s.last_update_slot = 0;
        s.bump = ctx.bumps.snapshot;
        Ok(())
    }

    pub fn push_nav(ctx: Context<PushNav>, entries: Vec<NavEntry>) -> Result<()> {
        require!(entries.len() <= 20, OracleError::TooManyEntries);
        let s = &mut ctx.accounts.snapshot;
        require!(
            s.authorized_keepers
                .iter()
                .any(|k| k == &ctx.accounts.authority.key()),
            OracleError::Unauthorized
        );
        s.entries = entries;
        s.last_update_slot = Clock::get()?.slot;
        Ok(())
    }

    pub fn set_keeper_authorization(
        ctx: Context<UpdateSnapshotConfig>,
        keeper: Pubkey,
        enabled: bool,
    ) -> Result<()> {
        let s = &mut ctx.accounts.snapshot;
        if enabled {
            if !s.authorized_keepers.iter().any(|k| k == &keeper) {
                s.authorized_keepers.push(keeper);
            }
        } else {
            s.authorized_keepers.retain(|k| k != &keeper);
        }
        Ok(())
    }

    pub fn set_max_stale_slots(
        ctx: Context<UpdateSnapshotConfig>,
        max_stale_slots: u64,
    ) -> Result<()> {
        require!(max_stale_slots > 0, OracleError::InvalidConfig);
        ctx.accounts.snapshot.max_stale_slots = max_stale_slots;
        Ok(())
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct NavEntry {
    pub mint: Pubkey,
    /// Price × 1e8, USD.
    pub price_usd_1e8: u64,
    /// Multiplier numerator; denominator is fixed at 1e18.
    pub multiplier_num: u128,
    /// xStocks asset status bitmask: bit0 = halted, bit1 = corp-action-pending.
    pub flags: u8,
    pub _padding: [u8; 7],
}

impl NavEntry {
    pub const SIZE: usize = 32 + 8 + 16 + 1 + 7;
}

#[account]
pub struct NavSnapshot {
    pub authority: Pubkey,
    pub authorized_keepers: Vec<Pubkey>,
    pub max_stale_slots: u64,
    pub sku: [u8; 16],
    pub last_update_slot: u64,
    pub entries: Vec<NavEntry>,
    pub bump: u8,
}

impl NavSnapshot {
    pub fn size(n: usize) -> usize {
        8 + 32 + 4 + 32 * 8 + 8 + 16 + 8 + 4 + n * NavEntry::SIZE + 1
    }
}

#[derive(Accounts)]
#[instruction(sku: [u8; 16])]
pub struct InitSnapshot<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,

    #[account(
        init,
        payer = authority,
        space = NavSnapshot::size(20),
        seeds = [b"nav", &sku[..]],
        bump,
    )]
    pub snapshot: Account<'info, NavSnapshot>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct PushNav<'info> {
    pub authority: Signer<'info>,

    #[account(
        mut,
        seeds = [b"nav", &snapshot.sku[..]],
        bump = snapshot.bump,
    )]
    pub snapshot: Account<'info, NavSnapshot>,
}

#[derive(Accounts)]
pub struct UpdateSnapshotConfig<'info> {
    pub authority: Signer<'info>,

    #[account(
        mut,
        seeds = [b"nav", &snapshot.sku[..]],
        bump = snapshot.bump,
        constraint = snapshot.authority == authority.key() @ OracleError::Unauthorized,
    )]
    pub snapshot: Account<'info, NavSnapshot>,
}

#[error_code]
pub enum OracleError {
    #[msg("Unauthorized")]
    Unauthorized,
    #[msg("Too many NAV entries (max 20)")]
    TooManyEntries,
    #[msg("Invalid oracle config")]
    InvalidConfig,
}
