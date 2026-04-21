#![allow(unexpected_cfgs)]

use anchor_lang::prelude::*;
use anchor_spl::token_interface::{
    transfer_checked, Mint, TokenAccount, TokenInterface, TransferChecked,
};
use sha2::{Sha256, Digest};

declare_id!("6T7wbptCbfmzrmdrLeSfKCXDxJMrqcrk1UxLRJBS7y8m");

/// xVault rewards — stake $VLT → receive epochal xStock-kind distributions.
///
/// Design: staker locks VLT for a tier (30 / 90 / 180 days) with NO boost multipliers.
/// All stakes earn pro-rata from epoch reward pools. Keeper runs `distribute_epoch` weekly,
/// crediting claims from a distribution vault pre-funded with xStocks from protocol fees.
#[program]
pub mod xvault_rewards {
    use super::*;

    pub fn init_pool(ctx: Context<InitPool>, args: InitPoolArgs) -> Result<()> {
        let p = &mut ctx.accounts.pool;
        p.admin = ctx.accounts.admin.key();
        p.vlt_mint = args.vlt_mint;
        p.reward_mint = args.reward_mint;
        p.total_weighted_stake = 0;
        p.current_epoch = 0;
        p.current_root = [0; 32];
        p.min_stake_age_secs = 7 * 86_400; // 7 days default
        p.bump = ctx.bumps.pool;
        Ok(())
    }

    pub fn stake(ctx: Context<Stake>, amount: u64, tier: LockTier) -> Result<()> {
        require!(amount > 0, RewardsError::ZeroAmount);
        require_keys_eq!(ctx.accounts.pool.vlt_mint, ctx.accounts.vlt_mint.key(), RewardsError::InvalidMint);

        let transfer_ctx = CpiContext::new(
            ctx.accounts.token_program.to_account_info(),
            TransferChecked {
                from: ctx.accounts.owner_vlt_ata.to_account_info(),
                mint: ctx.accounts.vlt_mint.to_account_info(),
                to: ctx.accounts.pool_vlt_ata.to_account_info(),
                authority: ctx.accounts.owner.to_account_info(),
            },
        );
        transfer_checked(transfer_ctx, amount, ctx.accounts.vlt_mint.decimals)?;

        let now = Clock::get()?.unix_timestamp;
        let current_slot = Clock::get()?.slot;
        let s = &mut ctx.accounts.stake_account;

        // Accumulate time-weighted balance before state change.
        if s.amount > 0 && s.last_update_slot > 0 {
            let slots_elapsed = current_slot.saturating_sub(s.last_update_slot);
            let accrual = (s.amount as u128)
                .checked_mul(slots_elapsed as u128)
                .ok_or(RewardsError::MathOverflow)?;
            s.cumulative_weighted_balance = s
                .cumulative_weighted_balance
                .checked_add(accrual)
                .ok_or(RewardsError::MathOverflow)?;
        }

        s.owner = ctx.accounts.owner.key();
        s.amount = s.amount.checked_add(amount).ok_or(RewardsError::MathOverflow)?;
        s.tier = tier;
        s.locked_until = now.checked_add(tier.duration_secs()).ok_or(RewardsError::MathOverflow)?;
        s.last_claim_epoch = ctx.accounts.pool.current_epoch;
        if s.stake_started_at == 0 {
            s.stake_started_at = now;
        }
        s.last_update_slot = current_slot;

        let weighted = (tier.weight_bps() as u64)
            .checked_mul(amount)
            .ok_or(RewardsError::MathOverflow)?
            .checked_div(10_000)
            .ok_or(RewardsError::MathOverflow)?;
        let pool = &mut ctx.accounts.pool;
        pool.total_weighted_stake = pool
            .total_weighted_stake
            .checked_add(weighted)
            .ok_or(RewardsError::MathOverflow)?;
        Ok(())
    }

    pub fn unstake(ctx: Context<Unstake>) -> Result<()> {
        let now = Clock::get()?.unix_timestamp;
        let current_slot = Clock::get()?.slot;
        require!(
            now >= ctx.accounts.stake_account.locked_until,
            RewardsError::StillLocked
        );
        require_keys_eq!(ctx.accounts.pool.vlt_mint, ctx.accounts.vlt_mint.key(), RewardsError::InvalidMint);

        let s = &mut ctx.accounts.stake_account;

        // Accumulate final time-weighted balance before zeroing.
        if s.amount > 0 && s.last_update_slot > 0 {
            let slots_elapsed = current_slot.saturating_sub(s.last_update_slot);
            let accrual = (s.amount as u128)
                .checked_mul(slots_elapsed as u128)
                .ok_or(RewardsError::MathOverflow)?;
            s.cumulative_weighted_balance = s
                .cumulative_weighted_balance
                .checked_add(accrual)
                .ok_or(RewardsError::MathOverflow)?;
        }
        s.last_update_slot = current_slot;

        let amount = s.amount;
        let weighted = (s.tier.weight_bps() as u64)
            .checked_mul(amount)
            .ok_or(RewardsError::MathOverflow)?
            .checked_div(10_000)
            .ok_or(RewardsError::MathOverflow)?;
        let pool = &mut ctx.accounts.pool;
        pool.total_weighted_stake = pool
            .total_weighted_stake
            .checked_sub(weighted)
            .ok_or(RewardsError::MathOverflow)?;

        let bump_seed = [pool.bump];
        let signer: &[&[u8]] = &[b"pool", &bump_seed];
        let signer_seeds = [signer];
        let transfer_ctx = CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            TransferChecked {
                from: ctx.accounts.pool_vlt_ata.to_account_info(),
                mint: ctx.accounts.vlt_mint.to_account_info(),
                to: ctx.accounts.owner_vlt_ata.to_account_info(),
                authority: pool.to_account_info(),
            },
            &signer_seeds,
        );
        transfer_checked(transfer_ctx, amount, ctx.accounts.vlt_mint.decimals)?;

        ctx.accounts.stake_account.amount = 0;
        Ok(())
    }

    pub fn distribute_epoch(
        ctx: Context<DistributeEpoch>,
        root: [u8; 32],
        total_reward_amount: u64,
    ) -> Result<()> {
        let pool = &mut ctx.accounts.pool;
        pool.current_epoch = pool
            .current_epoch
            .checked_add(1)
            .ok_or(RewardsError::MathOverflow)?;
        pool.current_root = root;

        emit!(EpochDistributedEvent {
            epoch: pool.current_epoch,
            root,
            total_reward_amount,
        });

        Ok(())
    }

    pub fn claim(ctx: Context<Claim>, amount: u64, proof: Vec<[u8; 32]>) -> Result<()> {        require!(amount > 0, RewardsError::ZeroAmount);
        let epoch = ctx.accounts.pool.current_epoch;
        require!(
            ctx.accounts.stake_account.last_claim_epoch < epoch,
            RewardsError::AlreadyClaimed
        );

        // Eligibility gate: stake must be at least min_stake_age_secs old.
        let now = Clock::get()?.unix_timestamp;
        let stake_age = now
            .checked_sub(ctx.accounts.stake_account.stake_started_at)
            .ok_or(RewardsError::MathOverflow)?;
        require!(
            stake_age >= ctx.accounts.pool.min_stake_age_secs,
            RewardsError::StakeNotOldEnough
        );

        // Check claimed bitmap to prevent replay.
        let bitmap = &mut ctx.accounts.claimed_bitmap;
        require!(!bitmap.claimed, RewardsError::AlreadyClaimed);

        // Verify Merkle proof: leaf = keccak256(abi.encodePacked(owner, epoch, amount)).
        let leaf = compute_leaf(
            &ctx.accounts.stake_account.owner,
            epoch,
            amount,
        );
        require!(
            verify_proof(&proof, &ctx.accounts.pool.current_root, &leaf),
            RewardsError::InvalidMerkleProof
        );

        // Mark as claimed.
        bitmap.claimed = true;

        // Transfer reward tokens from reward vault → owner.
        let pool = &ctx.accounts.pool;
        let bump_seed = [pool.bump];
        let signer: &[&[u8]] = &[b"pool", &bump_seed];
        let signer_seeds = [signer];
        let transfer_ctx = CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            TransferChecked {
                from: ctx.accounts.reward_vault_ata.to_account_info(),
                mint: ctx.accounts.reward_mint.to_account_info(),
                to: ctx.accounts.owner_reward_ata.to_account_info(),
                authority: ctx.accounts.pool.to_account_info(),
            },
            &signer_seeds,
        );
        transfer_checked(transfer_ctx, amount, ctx.accounts.reward_mint.decimals)?;

        ctx.accounts.stake_account.last_claim_epoch = epoch;

        emit!(ClaimEvent {
            owner: ctx.accounts.stake_account.owner,
            epoch,
            amount,
        });

        Ok(())
    }

    /// Reclaims rent from a fully-unstaked account. Requires `amount == 0` and
    /// `locked_until` to have elapsed (so callers cannot short-circuit an active lock).
    pub fn close_stake_account(_ctx: Context<CloseStakeAccount>) -> Result<()> {
        // Close constraint on the account is enforced by Anchor.
        Ok(())
    }
}

/* -------------------------------------------------------------------------- */
/*                               Merkle helpers                               */
/* -------------------------------------------------------------------------- */

fn compute_leaf(owner: &Pubkey, epoch: u64, amount: u64) -> [u8; 32] {
    let mut data = Vec::with_capacity(32 + 8 + 8);
    data.extend_from_slice(owner.as_ref());
    data.extend_from_slice(&epoch.to_le_bytes());
    data.extend_from_slice(&amount.to_le_bytes());
    let hash = Sha256::digest(&data);
    hash.into()
}

fn verify_proof(proof: &[[u8; 32]], root: &[u8; 32], leaf: &[u8; 32]) -> bool {
    let mut computed = *leaf;
    for node in proof {
        let mut combined = [0u8; 64];
        if computed <= *node {
            combined[..32].copy_from_slice(&computed);
            combined[32..].copy_from_slice(node);
        } else {
            combined[..32].copy_from_slice(node);
            combined[32..].copy_from_slice(&computed);
        }
        let hash = Sha256::digest(&combined);
        computed = hash.into();
    }
    computed == *root
}

/* -------------------------------------------------------------------------- */
/*                                   Types                                    */
/* -------------------------------------------------------------------------- */

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockTier {
    Days30,
    Days90,
    Days180,
}

impl LockTier {
    pub fn weight_bps(&self) -> u16 {
        10_000 // 1.0x for all tiers
    }
    pub fn duration_secs(&self) -> i64 {
        match self {
            LockTier::Days30 => 30 * 86_400,
            LockTier::Days90 => 90 * 86_400,
            LockTier::Days180 => 180 * 86_400,
        }
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct InitPoolArgs {
    pub vlt_mint: Pubkey,
    pub reward_mint: Pubkey,
}

/* -------------------------------------------------------------------------- */
/*                                  Accounts                                  */
/* -------------------------------------------------------------------------- */

#[account]
pub struct Pool {
    pub admin: Pubkey,
    pub vlt_mint: Pubkey,
    pub reward_mint: Pubkey,
    pub total_weighted_stake: u64,
    pub current_epoch: u64,
    pub current_root: [u8; 32],
    pub min_stake_age_secs: i64,
    pub bump: u8,
}

#[account]
pub struct StakeAccount {
    pub owner: Pubkey,
    pub amount: u64,
    pub tier: LockTier,
    pub locked_until: i64,
    pub last_claim_epoch: u64,
    pub cumulative_weighted_balance: u128,
    pub last_update_slot: u64,
    pub stake_started_at: i64,
}

/// Per-(epoch, recipient) bitmap that prevents replay claims.
#[account]
pub struct ClaimedBitmap {
    pub claimed: bool,
}

/* -------------------------------------------------------------------------- */
/*                                  Contexts                                  */
/* -------------------------------------------------------------------------- */

#[derive(Accounts)]
pub struct InitPool<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,

    #[account(
        init,
        payer = admin,
        space = 8 + 32 + 32 + 32 + 8 + 8 + 32 + 8 + 1,
        seeds = [b"pool"],
        bump,
    )]
    pub pool: Account<'info, Pool>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct Stake<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,

    #[account(
        mut,
        seeds = [b"pool"],
        bump = pool.bump,
    )]
    pub pool: Account<'info, Pool>,

    #[account(
        init_if_needed,
        payer = owner,
        space = 8 + 32 + 8 + 1 + 8 + 8 + 16 + 8 + 8,
        seeds = [b"stake", owner.key().as_ref()],
        bump,
    )]
    pub stake_account: Account<'info, StakeAccount>,

    pub vlt_mint: InterfaceAccount<'info, Mint>,

    #[account(
        mut,
        token::mint = vlt_mint,
        token::authority = owner,
        token::token_program = token_program,
    )]
    pub owner_vlt_ata: InterfaceAccount<'info, TokenAccount>,

    #[account(
        mut,
        token::mint = vlt_mint,
        token::authority = pool,
        token::token_program = token_program,
    )]
    pub pool_vlt_ata: InterfaceAccount<'info, TokenAccount>,

    pub token_program: Interface<'info, TokenInterface>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct Unstake<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,

    #[account(
        mut,
        seeds = [b"stake", owner.key().as_ref()],
        bump,
        constraint = stake_account.owner == owner.key() @ RewardsError::Unauthorized,
    )]
    pub stake_account: Account<'info, StakeAccount>,

    #[account(
        mut,
        seeds = [b"pool"],
        bump = pool.bump,
    )]
    pub pool: Account<'info, Pool>,

    pub vlt_mint: InterfaceAccount<'info, Mint>,

    #[account(
        mut,
        token::mint = vlt_mint,
        token::authority = owner,
        token::token_program = token_program,
    )]
    pub owner_vlt_ata: InterfaceAccount<'info, TokenAccount>,

    #[account(
        mut,
        token::mint = vlt_mint,
        token::authority = pool,
        token::token_program = token_program,
    )]
    pub pool_vlt_ata: InterfaceAccount<'info, TokenAccount>,

    pub token_program: Interface<'info, TokenInterface>,
}

#[derive(Accounts)]
pub struct DistributeEpoch<'info> {
    pub admin: Signer<'info>,

    #[account(
        mut,
        seeds = [b"pool"],
        bump = pool.bump,
        constraint = pool.admin == admin.key() @ RewardsError::Unauthorized,
    )]
    pub pool: Account<'info, Pool>,
}

#[derive(Accounts)]
pub struct Claim<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,

    #[account(
        seeds = [b"pool"],
        bump = pool.bump,
    )]
    pub pool: Account<'info, Pool>,

    #[account(
        mut,
        seeds = [b"stake", owner.key().as_ref()],
        bump,
        constraint = stake_account.owner == owner.key() @ RewardsError::Unauthorized,
    )]
    pub stake_account: Account<'info, StakeAccount>,

    /// Per-(epoch, recipient) claim bitmap to prevent replay.
    // SEEDS: [b"claimed", epoch.to_le_bytes(), owner]
    #[account(
        init_if_needed,
        payer = owner,
        space = 8 + 1,
        seeds = [b"claimed", pool.current_epoch.to_le_bytes().as_ref(), owner.key().as_ref()],
        bump,
    )]
    pub claimed_bitmap: Account<'info, ClaimedBitmap>,

    pub reward_mint: InterfaceAccount<'info, Mint>,

    /// Reward vault ATA (owned by pool PDA, holds reward tokens for distribution).
    #[account(
        mut,
        token::mint = reward_mint,
        token::authority = pool,
        token::token_program = token_program,
    )]
    pub reward_vault_ata: InterfaceAccount<'info, TokenAccount>,

    /// Owner's ATA for the reward token.
    #[account(
        mut,
        token::mint = reward_mint,
        token::authority = owner,
        token::token_program = token_program,
    )]
    pub owner_reward_ata: InterfaceAccount<'info, TokenAccount>,

    pub token_program: Interface<'info, TokenInterface>,

    pub system_program: Program<'info, System>,
}

/// Close a fully-unstaked account to reclaim rent.
/// Guards: amount must be zero, lock period must be elapsed.
#[derive(Accounts)]
pub struct CloseStakeAccount<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,

    #[account(
        mut,
        close = owner,
        seeds = [b"stake", owner.key().as_ref()],
        bump,
        constraint = stake_account.owner == owner.key() @ RewardsError::Unauthorized,
        constraint = stake_account.amount == 0 @ RewardsError::StillLocked,
        constraint = Clock::get()?.unix_timestamp >= stake_account.locked_until @ RewardsError::StillLocked,
    )]
    pub stake_account: Account<'info, StakeAccount>,
}

/* -------------------------------------------------------------------------- */
/*                                   Events                                   */
/* -------------------------------------------------------------------------- */

#[event]
pub struct EpochDistributedEvent {
    pub epoch: u64,
    pub root: [u8; 32],
    pub total_reward_amount: u64,
}

#[event]
pub struct ClaimEvent {
    pub owner: Pubkey,
    pub epoch: u64,
    pub amount: u64,
}

/* -------------------------------------------------------------------------- */
/*                                   Errors                                   */
/* -------------------------------------------------------------------------- */

#[error_code]
pub enum RewardsError {
    #[msg("Unauthorized")]
    Unauthorized,
    #[msg("Zero amount")]
    ZeroAmount,
    #[msg("Token program mismatch")]
    TokenProgramMismatch,
    #[msg("Math overflow")]
    MathOverflow,
    #[msg("Already claimed this epoch")]
    AlreadyClaimed,
    #[msg("Stake is still locked")]
    StillLocked,
    #[msg("Invalid mint")]
    InvalidMint,
    #[msg("Stake age requirement not met")]
    StakeNotOldEnough,
    #[msg("Invalid Merkle proof")]
    InvalidMerkleProof,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_merkle_proof_verification() {
        let owner = Pubkey::new_unique();
        let epoch = 1u64;
        let amount = 1_000_000u64;

        let leaf = compute_leaf(&owner, epoch, amount);

        // Single-leaf tree: root == leaf.
        assert!(verify_proof(&[], &leaf, &leaf));

        // Two-leaf tree.
        let other_leaf = compute_leaf(&Pubkey::new_unique(), epoch, 500_000);
        let (left, right) = if leaf <= other_leaf {
            (leaf, other_leaf)
        } else {
            (other_leaf, leaf)
        };
        let mut combined = [0u8; 64];
        combined[..32].copy_from_slice(&left);
        combined[32..].copy_from_slice(&right);
        let hash = Sha256::digest(&combined);
        let root: [u8; 32] = hash.into();

        // Prove `leaf` with `other_leaf` as sibling.
        assert!(verify_proof(&[other_leaf], &root, &leaf));
        // Different amount should fail.
        let bad_leaf = compute_leaf(&owner, epoch, 999_999);
        assert!(!verify_proof(&[other_leaf], &root, &bad_leaf));
    }

    #[test]
    fn test_compute_leaf_deterministic() {
        let owner = Pubkey::new_unique();
        let a = compute_leaf(&owner, 5, 1_000);
        let b = compute_leaf(&owner, 5, 1_000);
        assert_eq!(a, b);
    }
}
