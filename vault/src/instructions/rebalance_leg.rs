use anchor_lang::prelude::*;
use anchor_lang::solana_program::sysvar::instructions::{
    self, load_current_index_checked, load_instruction_at_checked,
};
use anchor_spl::token_2022::ID as TOKEN_2022_PROGRAM_ID;
use anchor_spl::token_interface::{Mint, TokenAccount};

use crate::errors::VaultError;
use crate::state::Vault;

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct RebalanceLegArgs {
    pub from_mint: Pubkey,
    pub to_mint: Pubkey,
    pub raw_in: u64,
    /// Min raw-out expected after swap (slippage guard).
    pub min_raw_out: u64,
}

#[derive(Accounts)]
pub struct RebalanceLeg<'info> {
    pub keeper: Signer<'info>,

    #[account(
        mut,
        seeds = [b"vault", &vault.sku[..]],
        bump = vault.bump,
        has_one = keeper @ VaultError::Unauthorized,
    )]
    pub vault: Account<'info, Vault>,

    /// CHECK: Instruction sysvar used for sibling instruction introspection.
    #[account(address = instructions::ID)]
    pub instructions_sysvar: UncheckedAccount<'info>,
}

pub fn handler<'info>(
    ctx: Context<'_, '_, 'info, 'info, RebalanceLeg<'info>>,
    args: RebalanceLegArgs,
) -> Result<()> {
    require!(!ctx.accounts.vault.paused, VaultError::Paused);
    require!(args.raw_in > 0, VaultError::ZeroAmount);

    let vault = &ctx.accounts.vault;
    require!(
        vault.holding(&args.from_mint).is_some(),
        VaultError::AssetNotWhitelisted
    );
    require!(
        vault.holding(&args.to_mint).is_some(),
        VaultError::AssetNotWhitelisted
    );
    let vault_key = vault.key();
    let prev_from_raw = vault
        .holding(&args.from_mint)
        .ok_or(VaultError::AssetNotWhitelisted)?
        .raw_balance;
    let prev_to_raw = vault
        .holding(&args.to_mint)
        .ok_or(VaultError::AssetNotWhitelisted)?
        .raw_balance;

    // Remaining accounts are required as:
    // [from_vault_ata, to_vault_ata, from_mint_account, to_mint_account]
    require!(
        ctx.remaining_accounts.len() == 4,
        VaultError::InvalidRemainingAccounts
    );

    let from_vault_ata = InterfaceAccount::<TokenAccount>::try_from(&ctx.remaining_accounts[0])?;
    let to_vault_ata = InterfaceAccount::<TokenAccount>::try_from(&ctx.remaining_accounts[1])?;
    let from_mint_acc = InterfaceAccount::<Mint>::try_from(&ctx.remaining_accounts[2])?;
    let to_mint_acc = InterfaceAccount::<Mint>::try_from(&ctx.remaining_accounts[3])?;

    require_keys_eq!(from_mint_acc.key(), args.from_mint, VaultError::InvalidRemainingAccounts);
    require_keys_eq!(to_mint_acc.key(), args.to_mint, VaultError::InvalidRemainingAccounts);
    require_keys_eq!(from_vault_ata.mint, args.from_mint, VaultError::InvalidRemainingAccounts);
    require_keys_eq!(to_vault_ata.mint, args.to_mint, VaultError::InvalidRemainingAccounts);
    require_keys_eq!(from_vault_ata.owner, vault_key, VaultError::InvalidRemainingAccounts);
    require_keys_eq!(to_vault_ata.owner, vault_key, VaultError::InvalidRemainingAccounts);

    let post_from_raw = from_vault_ata.amount;
    let post_to_raw = to_vault_ata.amount;

    let spent_raw = prev_from_raw
        .checked_sub(post_from_raw)
        .ok_or(VaultError::RebalanceSettlementInvalid)?;
    require!(
        spent_raw == args.raw_in,
        VaultError::RebalanceSettlementInvalid
    );

    let received_raw = post_to_raw
        .checked_sub(prev_to_raw)
        .ok_or(VaultError::RebalanceSettlementInvalid)?;
    require!(
        received_raw >= args.min_raw_out,
        VaultError::SlippageExceeded
    );

    // Sibling-instruction check: ensure there is a Token-2022 TransferChecked into the
    // expected destination ATA for at least `min_raw_out`.
    let current_idx = load_current_index_checked(&ctx.accounts.instructions_sysvar)? as usize;
    let mut has_valid_settlement_ix = false;
    for idx in 0..=current_idx {
        let ix = load_instruction_at_checked(idx, &ctx.accounts.instructions_sysvar)?;
        if ix.accounts.len() < 3 {
            continue;
        }
        if ix.program_id != TOKEN_2022_PROGRAM_ID {
            continue;
        }

        // TransferChecked discriminator for SPL Token / Token-2022.
        if ix.data.len() < 10 || ix.data[0] != 12 {
            continue;
        }

        let destination = ix.accounts[2].pubkey;
        if destination != to_vault_ata.key() {
            continue;
        }

        let amount = u64::from_le_bytes([
            ix.data[1], ix.data[2], ix.data[3], ix.data[4], ix.data[5], ix.data[6], ix.data[7],
            ix.data[8],
        ]);
        if amount >= args.min_raw_out {
            has_valid_settlement_ix = true;
            break;
        }
    }

    require!(
        has_valid_settlement_ix,
        VaultError::RebalanceSettlementInvalid
    );

    let vault = &mut ctx.accounts.vault;
    vault
        .holding_mut(&args.from_mint)
        .ok_or(VaultError::AssetNotWhitelisted)?
        .raw_balance = post_from_raw;
    vault
        .holding_mut(&args.to_mint)
        .ok_or(VaultError::AssetNotWhitelisted)?
        .raw_balance = post_to_raw;

    Ok(())
}
