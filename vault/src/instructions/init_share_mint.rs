use anchor_lang::prelude::*;
use anchor_spl::token_interface::{Mint, TokenInterface};

use crate::errors::VaultError;
use crate::state::Vault;

/// Initializes the Token-2022 share mint PDA. Must be called once after `init_vault`.
/// The vault PDA is the mint authority.
#[derive(Accounts)]
pub struct InitShareMint<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,

    #[account(
        seeds = [b"vault", &vault.sku[..]],
        bump = vault.bump,
        has_one = admin @ VaultError::Unauthorized,
    )]
    pub vault: Account<'info, Vault>,

    // SEEDS: [b"share", &vault.sku[..]]
    #[account(
        init,
        payer = admin,
        mint::decimals = 6,
        mint::authority = vault,
        mint::token_program = token_program,
        seeds = [b"share", &vault.sku[..]],
        bump,
    )]
    pub share_mint: InterfaceAccount<'info, Mint>,

    pub token_program: Interface<'info, TokenInterface>,
    pub system_program: Program<'info, System>,
}

pub fn handler(ctx: Context<InitShareMint>) -> Result<()> {
    require_keys_eq!(
        ctx.accounts.share_mint.key(),
        ctx.accounts.vault.share_mint,
        VaultError::ShareMintAlreadyInitialized
    );

    emit!(ShareMintInitializedEvent {
        vault: ctx.accounts.vault.key(),
        share_mint: ctx.accounts.share_mint.key(),
    });

    Ok(())
}

#[event]
pub struct ShareMintInitializedEvent {
    pub vault: Pubkey,
    pub share_mint: Pubkey,
}
