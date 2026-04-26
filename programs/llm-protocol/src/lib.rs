use anchor_lang::prelude::*;
use anchor_spl::token::{self, Mint, Token, TokenAccount, Transfer};

declare_id!("Fdz1UdBtnjav1HBdyXxR7K43bgYs9DiqduULERXS9SJa");

// Multiplicador (1e12) para evitar pérdida de precisión en divisiones enteras
const PRECISION: u128 = 1_000_000_000_000;

#[program]
pub mod llm_protocol {
    use super::*;

    /// 1. Inicializa el protocolo (Ejecutado por tu Backend/Admin)
    pub fn initialize(ctx: Context<Initialize>, reward_rate: u64) -> Result<()> {
        let global_state = &mut ctx.accounts.global_state;
        
        // Configuraciones globales
        global_state.admin = ctx.accounts.admin.key();
        global_state.reward_mint = ctx.accounts.reward_mint.key();
        global_state.reward_vault_bump = ctx.bumps.reward_vault;
        global_state.reward_rate = reward_rate; // Tokens a emitir por segundo globalmente
        global_state.last_update_time = Clock::get()?.unix_timestamp;
        global_state.reward_per_usage_stored = 0;
        global_state.total_usage = 0;

        Ok(())
    }

    /// Actualiza el estado matemático global de recompensas (Simula la emisión de tokens por bloque/tiempo)
    fn update_global_reward(global_state: &mut Account<GlobalState>, current_time: i64) -> Result<()> {
        if global_state.total_usage == 0 {
            global_state.last_update_time = current_time;
            return Ok(());
        }

        let time_elapsed = current_time.saturating_sub(global_state.last_update_time) as u64;
        let rewards_generated = time_elapsed.checked_mul(global_state.reward_rate).unwrap();

        let reward_per_usage_delta = (rewards_generated as u128)
            .checked_mul(PRECISION)
            .unwrap()
            .checked_div(global_state.total_usage as u128)
            .unwrap();

        global_state.reward_per_usage_stored = global_state
            .reward_per_usage_stored
            .checked_add(reward_per_usage_delta)
            .unwrap();
        
        global_state.last_update_time = current_time;

        Ok(())
    }

    /// Calcula y guarda cuánto ha generado este usuario desde su última interacción
    fn update_user_reward(global_state: &Account<GlobalState>, user_state: &mut Account<UserState>) -> Result<()> {
        let pending_reward = (user_state.user_usage as u128)
            .checked_mul(
                global_state.reward_per_usage_stored
                .saturating_sub(user_state.user_reward_per_usage_paid)
            )
            .unwrap()
            .checked_div(PRECISION)
            .unwrap() as u64;

        user_state.rewards = user_state.rewards.checked_add(pending_reward).unwrap();
        user_state.user_reward_per_usage_paid = global_state.reward_per_usage_stored;

        Ok(())
    }

    /// 2. Registrar uso de un API Key. EXCLUSIVO DEL BACKEND.
    pub fn register_usage(ctx: Context<RegisterUsage>, amount: u64) -> Result<()> {
        let global_state = &mut ctx.accounts.global_state;
        let user_state = &mut ctx.accounts.user_state;
        let current_time = Clock::get()?.unix_timestamp;

        // 1. Actualiza acumulado global según el tiempo transcurrido
        update_global_reward(global_state, current_time)?;
        
        // 2. Calcula las recompensas de este usuario de manera retroactiva
        update_user_reward(global_state, user_state)?;
        
        // 3. Imprime este nuevo uso (se sumará en futuras matemáticas)
        global_state.total_usage = global_state.total_usage.checked_add(amount).unwrap();
        user_state.user_usage = user_state.user_usage.checked_add(amount).unwrap();

        Ok(())
    }

    /// 3. Fondeo (El backend inyecta tokens reales USDC/$LLM al contrato periódicamente)
    pub fn fund_reward_vault(ctx: Context<FundRewardVault>, amount: u64) -> Result<()> {
        let cpi_accounts = Transfer {
            from: ctx.accounts.admin_token_account.to_account_info(),
            to: ctx.accounts.reward_vault.to_account_info(),
            authority: ctx.accounts.admin.to_account_info(), 
        };
        let cpi_program = ctx.accounts.token_program.to_account_info();
        let cpi_ctx = CpiContext::new(cpi_program, cpi_accounts);
        
        token::transfer(cpi_ctx, amount)?;

        Ok(())
    }

    /// 4. Reclamar (El backend extrae los fondos generados por este User ID hacia su Hot Wallet central)
    pub fn claim_reward(ctx: Context<ClaimReward>) -> Result<()> {
        let global_state = &mut ctx.accounts.global_state;
        let user_state = &mut ctx.accounts.user_state;
        let current_time = Clock::get()?.unix_timestamp;

        update_global_reward(global_state, current_time)?;
        update_user_reward(global_state, user_state)?;

        let reward_to_claim = user_state.rewards;
        require!(reward_to_claim > 0, CustomError::NoRewardsToClaim);

        // Se resetean fondos pendientes a 0 del usuario (estado)
        user_state.rewards = 0;

        // CPI transfer del Vault PDA -> Backend Hot Wallet
        let global_bump = ctx.bumps.global_state;
        let seeds = &[b"global".as_ref(), &[global_bump]];
        let signer = &[&seeds[..]];

        let cpi_accounts = Transfer {
            from: ctx.accounts.reward_vault.to_account_info(),
            to: ctx.accounts.destination_account.to_account_info(),
            authority: global_state.to_account_info(), // El PDA firma
        };
        let cpi_program = ctx.accounts.token_program.to_account_info();
        let cpi_ctx = CpiContext::new_with_signer(cpi_program, cpi_accounts, signer);

        token::transfer(cpi_ctx, reward_to_claim)?;

        Ok(())
    }

    /// Acelerar o reducir la emisión global
    pub fn update_reward_rate(ctx: Context<UpdateRewardRate>, new_rate: u64) -> Result<()> {
        let global_state = &mut ctx.accounts.global_state;
        let current_time = Clock::get()?.unix_timestamp;

        update_global_reward(global_state, current_time)?;
        global_state.reward_rate = new_rate;

        Ok(())
    }
}

// =========================
// ======= ACCOUNTS ========
// =========================

#[derive(Accounts)]
pub struct Initialize<'info> {
    #[account(
        init,
        payer = admin,
        space = 8 + GlobalState::INIT_SPACE,
        seeds = [b"global"],
        bump
    )]
    pub global_state: Account<'info, GlobalState>,

    pub reward_mint: Account<'info, Mint>,

    #[account(
        init,
        payer = admin,
        token::mint = reward_mint,
        token::authority = global_state, // El GlobalState tiene autoridad sobre las monedas
        seeds = [b"reward_vault"],
        bump
    )]
    pub reward_vault: Account<'info, TokenAccount>,

    #[account(mut)]
    pub admin: Signer<'info>,
    
    pub system_program: Program<'info, System>,
    pub token_program: Program<'info, Token>,
    pub rent: Sysvar<'info, Rent>,
}

#[derive(Accounts)]
pub struct RegisterUsage<'info> {
    // Solo el Admin (backend) especificado al inicializar puede llamar a esta instruccion
    #[account(mut, has_one = admin)]
    pub global_state: Account<'info, GlobalState>,

    // PDA que guarda la contabilidad del usuario en la Blockchain.
    #[account(
        init_if_needed,
        payer = admin, // El backend paga el gas fee del almacenamiento en SOL
        space = 8 + UserState::INIT_SPACE,
        seeds = [b"user", user_pubkey.key().as_ref()],
        bump
    )]
    pub user_state: Account<'info, UserState>,
    pub user_pubkey: AccountInfo<'info>,

    #[account(mut)]
    pub admin: Signer<'info>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct FundRewardVault<'info> {
    #[account(mut, has_one = admin)]
    pub global_state: Account<'info, GlobalState>,

    #[account(
        mut,
        seeds = [b"reward_vault"],
        bump = global_state.reward_vault_bump
    )]
    pub reward_vault: Account<'info, TokenAccount>,

    #[account(mut)]
    pub admin_token_account: Account<'info, TokenAccount>,

    #[account(mut)]
    pub admin: Signer<'info>,

    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct ClaimReward<'info> {
    #[account(mut, has_one = admin)]
    pub global_state: Account<'info, GlobalState>,

    #[account(mut)]
    pub user_state: Account<'info, UserState>,

    #[account(
        mut,
        seeds = [b"reward_vault"],
        bump = global_state.reward_vault_bump
    )]
    pub reward_vault: Account<'info, TokenAccount>,

    #[account(mut)]
    pub destination_account: Account<'info, TokenAccount>,

    #[account(mut)]
    pub admin: Signer<'info>,
    
    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct UpdateRewardRate<'info> {
    #[account(mut, has_one = admin)]
    pub global_state: Account<'info, GlobalState>,

    #[account(mut)]
    pub admin: Signer<'info>,
}

#[account]
pub struct GlobalState {
    pub admin: Pubkey,
    pub reward_mint: Pubkey,
    pub reward_vault_bump: u8,
    pub reward_rate: u64, // Cuantos tokens się generan por segundo (escalable)
    pub last_update_time: i64, 
    pub reward_per_usage_stored: u128,
    pub total_usage: u64,
}

impl GlobalState {
    pub const INIT_SPACE: usize = 32 + 32 + 1 + 8 + 8 + 16 + 8;
}

#[account]
pub struct UserState {
    pub user_usage: u64,
    pub user_reward_per_usage_paid: u128,
    pub rewards: u64,
}

impl UserState {
    pub const INIT_SPACE: usize = 8 + 16 + 8;
}


#[error_code]
pub enum CustomError {
    #[msg("Este usuario aun no ha generado recompensas")]
    NoRewardsToClaim,
}
