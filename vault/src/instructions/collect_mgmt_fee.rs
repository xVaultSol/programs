use anchor_lang::prelude::*;
use anchor_spl::token_interface::{mint_to, Mint, MintTo, TokenAccount, TokenInterface};

use crate::errors::VaultError;
use crate::state::{Vault, SECS_PER_YEAR, BPS_DENOM};

/// Permissionless — anyone can call this to collect accrued management fees.
/// Mints new share tokens to the treasury proportional to time elapsed × fee rate.
///
/// fee_shares = total_supply × mgmt_fee_bps × elapsed_secs / (BPS_DENOM × SECS_PER_YEAR)
#[derive(Accounts)]
pub struct CollectMgmtFee<'info> {
    /// Anyone can crank this.
    pub caller: Signer<'info>,

    #[account(
        mut,
        seeds = [b"vault", &vault.sku[..]],
        bump = vault.bump,
    )]
    pub vault: Account<'info, Vault>,

    #[account(
        mut,
        address = vault.share_mint,
    )]
    pub share_mint: InterfaceAccount<'info, Mint>,

    /// Treasury share ATA that receives minted fee shares.
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

pub fn handler(ctx: Context<CollectMgmtFee>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let vault = &ctx.accounts.vault;
    let elapsed = now
        .checked_sub(vault.last_fee_collection_ts)
        .ok_or(VaultError::MathOverflow)?;
    require!(elapsed > 0, VaultError::NoFeeAccrued);

    let total_supply = ctx.accounts.share_mint.supply;
    if total_supply == 0 {
        // No shares outstanding → no fee to collect, just update timestamp.
        ctx.accounts.vault.last_fee_collection_ts = now;
        return Ok(());
    }

    // fee_shares = total_supply × mgmt_fee_bps × elapsed / (BPS_DENOM × SECS_PER_YEAR)
    let fee_shares = (total_supply as u128)
        .checked_mul(vault.management_fee_bps as u128)
        .ok_or(VaultError::MathOverflow)?
        .checked_mul(elapsed as u128)
        .ok_or(VaultError::MathOverflow)?
        .checked_div(
            BPS_DENOM
                .checked_mul(SECS_PER_YEAR)
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

    ctx.accounts.vault.last_fee_collection_ts = now;

    emit!(MgmtFeeCollectedEvent {
        vault: ctx.accounts.vault.key(),
        fee_shares: fee_shares_u64,
        elapsed_secs: elapsed,
    });

    Ok(())
}

#[event]
pub struct MgmtFeeCollectedEvent {
    pub vault: Pubkey,
    pub fee_shares: u64,
    pub elapsed_secs: i64,
}
