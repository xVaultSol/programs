use anchor_lang::prelude::*;

pub const MAX_ASSETS: usize = 20;
pub const BPS_DENOMINATOR: u16 = 10_000;

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
    /// Human-readable SKU (e.g. "MAG7_V1"); first 16 bytes used as PDA seed.
    pub sku: [u8; 16],
    /// Whitelisted basket.
    pub holdings: Vec<Holding>,
    /// USDC buffer (raw, 6 decimals).
    pub cash_raw: u64,
    /// Management fee, bps per year. Accrued offline; collected via separate ix.
    pub management_fee_bps: u16,
    /// Max per-leg NAV drift allowed during a rebalance, bps.
    pub rebalance_slippage_bps: u16,
    /// When true, deposits + rebalances blocked.
    pub paused: bool,
    /// PDA bump for the Vault account.
    pub bump: u8,
    /// PDA bump for the share mint.
    pub share_mint_bump: u8,
    pub _padding: [u8; 5],
}

impl Vault {
    pub const BASE_SIZE: usize =
        32 + 32 + 32 + 32 + 16 + 4 /* vec len */ + 8 + 2 + 2 + 1 + 1 + 1 + 5;

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
