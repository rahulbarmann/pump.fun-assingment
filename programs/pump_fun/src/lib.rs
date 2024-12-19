use anchor_lang::prelude::*;
use anchor_spl::token::{self, Token, Transfer};

declare_id!("BDmyUtXfoCXubpBTscdVFRGrvu6RN6geGTSypRm4BbwQ");

pub const DECIMALS: u8 = 9;
pub const DECIMAL_MULTIPLIER: u64 = 1_000_000_000; // 10^9
pub const INITIAL_MARKET_CAP: u64 = 42_000; // $42,000
pub const GRADUATION_MARKET_CAP: u64 = 100_000; // $100,000
pub const TOTAL_SUPPLY: u64 = 1_000_000_000; // 1 billion tokens
pub const LP_ALLOCATION_PERCENTAGE: u8 = 20; // 20% for LP
pub const FEE_PERCENTAGE: u8 = 1; // 1% fee
pub const MAX_PRICE_MULTIPLIER: u8 = 3; // 3x max price before graduation

#[program]
pub mod pump_fun {
    use super::*;

    pub fn initialize(
        ctx: Context<Initialize>,
        fee_wallet: Pubkey,
        owner: Pubkey
    ) -> Result<()> {
        let state = &mut ctx.accounts.state;
        state.fee_wallet = fee_wallet;
        state.owner = owner;
        state.market_cap = INITIAL_MARKET_CAP;
        state.circulating_supply = 0;
        state.graduated = false;
        state.initial_price = INITIAL_MARKET_CAP / TOTAL_SUPPLY;
        state.current_price = state.initial_price;
        state.lp_tokens_locked = false;
        Ok(())
    }

    pub fn update_fee_wallet(ctx: Context<UpdateFeeWallet>, new_fee_wallet: Pubkey) -> Result<()> {
        require!(
            ctx.accounts.owner.key() == ctx.accounts.state.owner,
            PumpFunError::UnauthorizedOperation
        );
        ctx.accounts.state.fee_wallet = new_fee_wallet;
        Ok(())
    }

    pub fn buy(ctx: Context<Buy>, amount: u64) -> Result<()> {
        let state = &mut ctx.accounts.state;
        
        // Calculate price using bonding curve
        let price = calculate_price(state.circulating_supply)?;
        
        // Check price restrictions before graduation
        if !state.graduated {
            require!(
                price <= state.initial_price.checked_mul(MAX_PRICE_MULTIPLIER as u64)
                    .ok_or::<Error>(PumpFunError::ArithmeticError.into())?,
                PumpFunError::PriceExceedsLimit
            );
        }

        // Calculate total cost and fees
        let total_cost = price
            .checked_mul(amount)
            .ok_or::<Error>(PumpFunError::ArithmeticError.into())?;
        let fee_amount = total_cost
            .checked_mul(FEE_PERCENTAGE as u64)
            .ok_or::<Error>(PumpFunError::ArithmeticError.into())?
            .checked_div(100)
            .ok_or::<Error>(PumpFunError::ArithmeticError.into())?;

        let transfer_amount = total_cost
            .checked_sub(fee_amount)
            .ok_or::<Error>(PumpFunError::ArithmeticError.into())?
            .checked_mul(DECIMAL_MULTIPLIER)
            .ok_or::<Error>(PumpFunError::ArithmeticError.into())?;

        // Transfer main amount
        token::transfer(
            CpiContext::new(
                ctx.accounts.token_program.to_account_info(),
                Transfer {
                    from: ctx.accounts.buyer_token_account.to_account_info(),
                    to: ctx.accounts.recipient_token_account.to_account_info(),
                    authority: ctx.accounts.buyer.to_account_info(),
                },
            ),
            transfer_amount,
        )?;

        // Transfer fee
        token::transfer(
            CpiContext::new(
                ctx.accounts.token_program.to_account_info(),
                Transfer {
                    from: ctx.accounts.buyer_token_account.to_account_info(),
                    to: ctx.accounts.fee_token_account.to_account_info(),
                    authority: ctx.accounts.buyer.to_account_info(),
                },
            ),
            fee_amount.checked_mul(DECIMAL_MULTIPLIER).ok_or::<Error>(PumpFunError::ArithmeticError.into())?,
        )?;

        // Update state
        state.circulating_supply = state.circulating_supply
            .checked_add(amount)
            .ok_or::<Error>(PumpFunError::ArithmeticError.into())?;
        state.current_price = price;
        state.market_cap = state.circulating_supply
            .checked_mul(price)
            .ok_or::<Error>(PumpFunError::ArithmeticError.into())?;

        // Check graduation conditions
        if !state.graduated && state.market_cap >= GRADUATION_MARKET_CAP {
            state.graduated = true;
            
            // Calculate LP amount
            let lp_amount = TOTAL_SUPPLY
                .checked_mul(LP_ALLOCATION_PERCENTAGE as u64)
                .ok_or::<Error>(PumpFunError::ArithmeticError.into())?
                .checked_div(100)
                .ok_or::<Error>(PumpFunError::ArithmeticError.into())?;

            // Move tokens to LP directly within the buy instruction
            token::transfer(
                CpiContext::new(
                    ctx.accounts.token_program.to_account_info(),
                    Transfer {
                        from: ctx.accounts.buyer_token_account.to_account_info(),
                        to: ctx.accounts.recipient_token_account.to_account_info(),
                        authority: ctx.accounts.buyer.to_account_info(),
                    },
                ),
                lp_amount,
            )?;
        }

        // Emit event for frontend tracking
        emit!(TransactionEvent {
            transaction_type: TransactionType::Buy,
            amount,
            price,
            market_cap: state.market_cap,
            timestamp: Clock::get()?.unix_timestamp,
        });

        Ok(())
    }

    pub fn sell(ctx: Context<Sell>, amount: u64) -> Result<()> {
        let state = &mut ctx.accounts.state;
        
        // Verify we have enough supply
        require!(
            state.circulating_supply >= amount,
            PumpFunError::InsufficientSupply
        );
        
        // Calculate price using bonding curve
        let price = calculate_price(state.circulating_supply.checked_sub(amount)
            .ok_or::<Error>(PumpFunError::ArithmeticError.into())?)?;
        
        // Calculate total payout and fees
        let total_payout = price
            .checked_mul(amount)
            .ok_or::<Error>(PumpFunError::ArithmeticError.into())?;
        
        let fee_amount = total_payout
            .checked_mul(FEE_PERCENTAGE as u64)
            .ok_or::<Error>(PumpFunError::ArithmeticError.into())?
            .checked_div(100)
            .ok_or::<Error>(PumpFunError::ArithmeticError.into())?;

        // Calculate net payout after fees
        let net_payout = total_payout
            .checked_sub(fee_amount)
            .ok_or::<Error>(PumpFunError::ArithmeticError.into())?
            .checked_mul(DECIMAL_MULTIPLIER)
            .ok_or::<Error>(PumpFunError::ArithmeticError.into())?;

        // Transfer tokens from seller to contract
        token::transfer(
            CpiContext::new(
                ctx.accounts.token_program.to_account_info(),
                Transfer {
                    from: ctx.accounts.seller_token_account.to_account_info(),
                    to: ctx.accounts.contract_token_account.to_account_info(),
                    authority: ctx.accounts.seller.to_account_info(),
                },
            ),
            amount,
        )?;

        // Transfer payment to seller
        token::transfer(
            CpiContext::new(
                ctx.accounts.token_program.to_account_info(),
                Transfer {
                    from: ctx.accounts.payment_token_account.to_account_info(),
                    to: ctx.accounts.seller_payment_account.to_account_info(),
                    authority: ctx.accounts.contract.to_account_info(),
                },
            ),
            net_payout,
        )?;

        // Transfer fee
        token::transfer(
            CpiContext::new(
                ctx.accounts.token_program.to_account_info(),
                Transfer {
                    from: ctx.accounts.payment_token_account.to_account_info(),
                    to: ctx.accounts.fee_token_account.to_account_info(),
                    authority: ctx.accounts.contract.to_account_info(),
                },
            ),
            fee_amount.checked_mul(DECIMAL_MULTIPLIER)
                .ok_or::<Error>(PumpFunError::ArithmeticError.into())?,
        )?;

        // Update state
        state.circulating_supply = state.circulating_supply
            .checked_sub(amount)
            .ok_or::<Error>(PumpFunError::ArithmeticError.into())?;
        state.current_price = price;
        state.market_cap = state.circulating_supply
            .checked_mul(price)
            .ok_or::<Error>(PumpFunError::ArithmeticError.into())?;

        // Emit event for frontend tracking
        emit!(TransactionEvent {
            transaction_type: TransactionType::Sell,
            amount,
            price,
            market_cap: state.market_cap,
            timestamp: Clock::get()?.unix_timestamp,
        });

        Ok(())
    }

    pub fn lock_lp_tokens(ctx: Context<LockLP>) -> Result<()> {
        let state = &mut ctx.accounts.state;
        require!(state.graduated, PumpFunError::NotGraduated);
        require!(!state.lp_tokens_locked, PumpFunError::LPAlreadyLocked);
        
        state.lp_tokens_locked = true;
        Ok(())
    }
}

#[derive(Accounts)]
pub struct Sell<'info> {
    #[account(mut)]
    pub state: Account<'info, PumpFunState>,
    pub seller: Signer<'info>,
    pub contract: Signer<'info>,
    #[account(mut)]
    pub seller_token_account: AccountInfo<'info>,
    #[account(mut)]
    pub contract_token_account: AccountInfo<'info>,
    #[account(mut)]
    pub payment_token_account: AccountInfo<'info>,
    #[account(mut)]
    pub seller_payment_account: AccountInfo<'info>,
    #[account(mut)]
    pub fee_token_account: AccountInfo<'info>,
    pub token_program: Program<'info, Token>,
}


#[derive(Accounts)]
pub struct Initialize<'info> {
    #[account(init, payer = payer, space = 8 + 32 + 32 + 8 + 8 + 1 + 8 + 8 + 1)]
    pub state: Account<'info, PumpFunState>,
    #[account(mut)]
    pub payer: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct UpdateFeeWallet<'info> {
    #[account(mut)]
    pub state: Account<'info, PumpFunState>,
    pub owner: Signer<'info>,
}

#[derive(Accounts)]
pub struct Buy<'info> {
    #[account(mut)]
    pub state: Account<'info, PumpFunState>,
    pub buyer: Signer<'info>,
    #[account(mut)]
    pub buyer_token_account: AccountInfo<'info>,
    #[account(mut)]
    pub recipient_token_account: AccountInfo<'info>,
    #[account(mut)]
    pub fee_token_account: AccountInfo<'info>,
    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct LockLP<'info> {
    #[account(mut)]
    pub state: Account<'info, PumpFunState>,
    pub owner: Signer<'info>,
}

#[account]
pub struct PumpFunState {
    pub fee_wallet: Pubkey,
    pub owner: Pubkey,
    pub market_cap: u64,
    pub circulating_supply: u64,
    pub graduated: bool,
    pub initial_price: u64,
    pub current_price: u64,
    pub lp_tokens_locked: bool,
}

#[error_code]
pub enum PumpFunError {
    #[msg("Arithmetic error occurred")]
    ArithmeticError,
    #[msg("Price exceeds maximum allowed before graduation")]
    PriceExceedsLimit,
    #[msg("Unauthorized operation")]
    UnauthorizedOperation,
    #[msg("Token has not graduated yet")]
    NotGraduated,
    #[msg("LP tokens are already locked")]
    LPAlreadyLocked,
    #[msg("Insufficient supply for sell operation")]
    InsufficientSupply,

}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, PartialEq, Eq)]
pub enum TransactionType {
    Buy,
    Sell,
}

#[event]
pub struct TransactionEvent {
    pub transaction_type: TransactionType,
    pub amount: u64,
    pub price: u64,
    pub market_cap: u64,
    pub timestamp: i64,
}

// Helper function for price calculation
fn calculate_price(current_supply: u64) -> Result<u64> {
    // Simple linear bonding curve: price increases linearly with supply
    let base_price = INITIAL_MARKET_CAP / TOTAL_SUPPLY;
    let supply_percentage = current_supply
        .checked_mul(100)
        .ok_or::<Error>(PumpFunError::ArithmeticError.into())?
        .checked_div(TOTAL_SUPPLY)
        .ok_or::<Error>(PumpFunError::ArithmeticError.into())?;
    
    let price_multiplier = 100u64.checked_add(supply_percentage)
        .ok_or::<Error>(PumpFunError::ArithmeticError.into())?;
    
    base_price
        .checked_mul(price_multiplier)
        .ok_or::<Error>(PumpFunError::ArithmeticError.into())?
        .checked_div(100)
        .ok_or::<Error>(PumpFunError::ArithmeticError.into())
}