#![allow(unexpected_cfgs)]

use anchor_lang::prelude::*;
use anchor_spl::token_interface::{
    transfer_checked, Mint, TokenAccount, TokenInterface, TransferChecked,
};

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
        s.owner = ctx.accounts.owner.key();
        s.amount = s.amount.checked_add(amount).ok_or(RewardsError::MathOverflow)?;
        s.tier = tier;
        s.locked_until = now.checked_add(tier.duration_secs()).ok_or(RewardsError::MathOverflow)?;
        s.last_claim_epoch = ctx.accounts.pool.current_epoch;
        // Initialize time-weighted balance on first stake
        if s.stake_started_at == 0 {
            s.stake_started_at = now;
        }
        s.last_update_slot = current_slot;
        s.cumulative_weighted_balance = 0; // Will accumulate each update
        let weighted = tier.weight_bps() as u64 * amount / 10_000; // All tiers are 1.0x now
        let pool = &mut ctx.accounts.pool;
        pool.total_weighted_stake = pool
            .total_weighted_stake
            .checked_add(weighted)
            .ok_or(RewardsError::MathOverflow)?;
        Ok(())
    }

    pub fn unstake(ctx: Context<Unstake>) -> Result<()> {
        let now = Clock::get()?.unix_timestamp;
        require!(
            now >= ctx.accounts.stake_account.locked_until,
            RewardsError::StillLocked
        );
        require_keys_eq!(ctx.accounts.pool.vlt_mint, ctx.accounts.vlt_mint.key(), RewardsError::InvalidMint);

        let amount = ctx.accounts.stake_account.amount;
        let weighted = ctx.accounts.stake_account.tier.weight_bps() as u64 * amount / 10_000;
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
                authority: ctx.accounts.pool.to_account_info(),
            },
            &signer_seeds,
        );
        transfer_checked(transfer_ctx, amount, ctx.accounts.vlt_mint.decimals)?;

        ctx.accounts.stake_account.amount = 0;
        Ok(())
    }

    pub fn distribute_epoch(ctx: Context<DistributeEpoch>, root: [u8; 32]) -> Result<()> {
        let pool = &mut ctx.accounts.pool;
        pool.current_epoch = pool
            .current_epoch
            .checked_add(1)
            .ok_or(RewardsError::MathOverflow)?;
        pool.current_root = root;
        Ok(())
    }

    pub fn claim(ctx: Context<Claim>, amount: u64) -> Result<()> {
        require!(amount > 0, RewardsError::ZeroAmount);
        let epoch = ctx.accounts.pool.current_epoch;
        require!(ctx.accounts.stake_account.last_claim_epoch < epoch, RewardsError::AlreadyClaimed);
        
        // Eligibility gate: stake must be at least min_stake_age_secs old
        let now = Clock::get()?.unix_timestamp;
        let stake_age = now.checked_sub(ctx.accounts.stake_account.stake_started_at)
            .ok_or(RewardsError::MathOverflow)?;
        require!(stake_age >= ctx.accounts.pool.min_stake_age_secs, RewardsError::StakeNotOldEnough);

        // TODO(next): verify merkle proof against pool.current_root.
        ctx.accounts.stake_account.last_claim_epoch = epoch;
        Ok(())
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockTier {
    Days30,
    Days90,
    Days180,
}

impl LockTier {
    pub fn weight_bps(&self) -> u16 {
        // All tiers earn pro-rata (no boost multipliers)
        10_000 // 1.0x for all
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
}

#[account]
pub struct Pool {
    pub admin: Pubkey,
    pub vlt_mint: Pubkey,
    pub total_weighted_stake: u64,
    pub current_epoch: u64,
    pub current_root: [u8; 32],
    pub min_stake_age_secs: i64,  // e.g., 7 days = 604_800
    pub bump: u8,
}

#[account]
pub struct StakeAccount {
    pub owner: Pubkey,
    pub amount: u64,
    pub tier: LockTier,
    pub locked_until: i64,
    pub last_claim_epoch: u64,
    // Time-weighted balance tracking (anti-gaming)
    pub cumulative_weighted_balance: u128,
    pub last_update_slot: u64,
    pub stake_started_at: i64,  // unix timestamp when stake was created
}

#[derive(Accounts)]
pub struct InitPool<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,

    #[account(
        init,
        payer = admin,
        space = 8 + 32 + 32 + 8 + 8 + 8 + 1,
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
}

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
}
