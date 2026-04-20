use anchor_lang::prelude::*;
use anchor_spl::token_interface::{
    burn, transfer_checked, Burn, Mint, TokenAccount, TokenInterface, TransferChecked,
};

use crate::errors::VaultError;
use crate::state::Vault;

#[derive(Accounts)]
pub struct WithdrawInKind<'info> {
    #[account(mut)]
    pub user: Signer<'info>,

    #[account(
        mut,
        seeds = [b"vault", &vault.sku[..]],
        bump = vault.bump,
    )]
    pub vault: Account<'info, Vault>,

    #[account(mut, address = vault.share_mint)]
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

pub fn handler<'info>(
    ctx: Context<'_, '_, 'info, 'info, WithdrawInKind<'info>>,
    shares: u64,
) -> Result<()> {
    require!(shares > 0, VaultError::ZeroAmount);

    let total_shares = ctx.accounts.share_mint.supply;
    require!(total_shares > 0, VaultError::MathOverflow);

    // Burn user shares.
    let burn_ctx = CpiContext::new(
        ctx.accounts.token_program.to_account_info(),
        Burn {
            mint: ctx.accounts.share_mint.to_account_info(),
            from: ctx.accounts.user_share_ata.to_account_info(),
            authority: ctx.accounts.user.to_account_info(),
        },
    );
    burn(burn_ctx, shares)?;

    // Remaining accounts must be grouped per holding as:
    // [vault_holding_ata, user_holding_ata, holding_mint] triplets.
    let holdings_len = ctx.accounts.vault.holdings.len();
    require!(
        ctx.remaining_accounts.len() == holdings_len * 3,
        VaultError::InvalidRemainingAccounts
    );

    let vault_key = ctx.accounts.vault.key();
    let vault_sku = ctx.accounts.vault.sku;
    let vault_bump_seed = [ctx.accounts.vault.bump];
    let signer_seeds: &[&[u8]] = &[b"vault", &vault_sku, &vault_bump_seed];
    let signer = [signer_seeds];
    let token_program = ctx.accounts.token_program.to_account_info();
    let vault_info = ctx.accounts.vault.to_account_info();

    for i in 0..holdings_len {
        let (holding_mint, holding_decimals, holding_raw_balance) = {
            let holding = &ctx.accounts.vault.holdings[i];
            (holding.mint, holding.decimals, holding.raw_balance)
        };

        let base = i.checked_mul(3).ok_or(VaultError::MathOverflow)?;
        let idx1 = base.checked_add(1).ok_or(VaultError::MathOverflow)?;
        let idx2 = base.checked_add(2).ok_or(VaultError::MathOverflow)?;
        let vault_ata = InterfaceAccount::<TokenAccount>::try_from(&ctx.remaining_accounts[base])?;
        let user_ata = InterfaceAccount::<TokenAccount>::try_from(&ctx.remaining_accounts[idx1])?;
        let mint_acc = InterfaceAccount::<Mint>::try_from(&ctx.remaining_accounts[idx2])?;

        require_keys_eq!(mint_acc.key(), holding_mint, VaultError::InvalidRemainingAccounts);
        require_keys_eq!(vault_ata.mint, holding_mint, VaultError::InvalidRemainingAccounts);
        require_keys_eq!(user_ata.mint, holding_mint, VaultError::InvalidRemainingAccounts);
        require_keys_eq!(vault_ata.owner, vault_key, VaultError::InvalidRemainingAccounts);

        let raw_out = (holding_raw_balance as u128)
            .checked_mul(shares as u128)
            .ok_or(VaultError::MathOverflow)?
            .checked_div(total_shares as u128)
            .ok_or(VaultError::MathOverflow)? as u64;

        if raw_out > 0 {
            let transfer_ctx = CpiContext::new_with_signer(
                token_program.clone(),
                TransferChecked {
                    from: ctx.remaining_accounts[base].clone(),
                    mint: ctx.remaining_accounts[idx2].clone(),
                    to: ctx.remaining_accounts[idx1].clone(),
                    authority: vault_info.clone(),
                },
                &signer,
            );
            transfer_checked(transfer_ctx, raw_out, holding_decimals)?;
        }

        let holding = &mut ctx.accounts.vault.holdings[i];
        holding.raw_balance = holding
            .raw_balance
            .checked_sub(raw_out)
            .ok_or(VaultError::MathOverflow)?;
    }

    Ok(())
}
