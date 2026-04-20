use anchor_lang::prelude::*;

pub const MAX_ASSETS: usize = 20;
pub const BPS_DENOMINATOR: u16 = 10_000;
pub const SECS_PER_YEAR: u128 = 365 * 24 * 3600;
pub const WITHDRAW_FEE_BPS: u128 = 5; // 0.05%
pub const BPS_DENOM: u128 = 10_000;

/// Per-asset holding descriptor embedded in the `Vault` account.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Holding {
    /// Token-2022 mint of the xStock.
    pub mint: Pubkey,
    /// Target weight in bps (0..=10_000). Sum across holdings == 10_000.
    pub target_weight_bps: u16,
    /// Raw on-chain balance (pre-scaled). Updated on every deposit / withdraw / rebalance.
    pub raw_balance: u64,
    /// Mint decimals (cached to avoid cross-program reads during math).
    pub decimals: u8,
    /// Pad to 8-byte alignment.
    pub _padding: [u8; 5],
}

impl Holding {
    pub const SIZE: usize = 32 + 2 + 8 + 1 + 5;
}

#[account]
#[derive(Debug)]
pub struct Vault {
    /// Squads multisig or keypair that can update weights / pause.
    pub admin: Pubkey,
    /// Keeper authority allowed to call `rebalance_leg`.
    pub keeper: Pubkey,
    /// Oracle program-derived NAV snapshot account.
    pub nav_snapshot: Pubkey,
    /// LP share mint (Token-2022).
    pub share_mint: Pubkey,
    /// Treasury ATA that receives fee shares.
    pub treasury: Pubkey,
    /// Human-readable SKU (e.g. "MAG7_V1"); first 16 bytes used as PDA seed.
    pub sku: [u8; 16],
    /// Whitelisted basket.
    pub holdings: Vec<Holding>,
    /// USDC buffer (raw, 6 decimals).
    pub cash_raw: u64,
    /// Management fee, bps per year. Accrued offline; collected via separate ix.
    pub management_fee_bps: u16,
    /// Performance fee, bps of positive NAV delta (default 1000 = 10%).
    pub performance_fee_bps: u16,
    /// Max per-leg NAV drift allowed during a rebalance, bps.
    pub rebalance_slippage_bps: u16,
    /// Last unix timestamp when management fee was collected.
    pub last_fee_collection_ts: i64,
    /// High-water mark NAV per share in 1e8 for performance fee.
    pub hwm_nav_per_share_1e8: u128,
    /// Accumulated protocol USDC fees (raw, 6 decimals) from withdrawals.
    pub accrued_protocol_fees_raw: u64,
    /// When true, deposits + rebalances blocked.
    pub paused: bool,
    /// PDA bump for the Vault account.
    pub bump: u8,
    /// PDA bump for the share mint.
    pub share_mint_bump: u8,
    pub _padding: [u8; 3],
}

impl Vault {
    pub const BASE_SIZE: usize =
        32 + 32 + 32 + 32 + 32 /* treasury */ + 16 + 4 /* vec len */ + 8 + 2 + 2 + 2 + 8 + 16 + 8 + 1 + 1 + 1 + 3;

    pub fn size(basket_len: usize) -> usize {
        8 /* disc */ + Self::BASE_SIZE + basket_len * Holding::SIZE
    }

    pub fn holding_mut(&mut self, mint: &Pubkey) -> Option<&mut Holding> {
        self.holdings.iter_mut().find(|h| &h.mint == mint)
    }

    pub fn holding(&self, mint: &Pubkey) -> Option<&Holding> {
        self.holdings.iter().find(|h| &h.mint == mint)
    }
}
