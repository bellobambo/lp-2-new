use anchor_lang::prelude::*;
use anchor_lang::system_program;
use anchor_lang::solana_program::{system_instruction, program::invoke_signed};

declare_id!("AkDSbrdvrnfe558WDZEkGuJUayt8nChyog6bcGr1hVFm");

#[program]
pub mod lp_program {
    use super::*;

    // Client posts a job offer and locks funds in escrow
    #[allow(clippy::too_many_arguments)]
    pub fn initialize_job_post(
        ctx: Context<InitializeJobPost>,
        title: String,
        description: String,
        amount: u64,
        start_date: i64,
        end_date: i64,
    ) -> Result<()> {
        require!(!title.is_empty(), ErrorCode::InvalidInput);
        require!(!description.is_empty(), ErrorCode::InvalidInput);
        require!(amount > 0, ErrorCode::InvalidAmount);
        require!(start_date <= end_date, ErrorCode::InvalidDates);

        let clock = Clock::get()?;
        require!(start_date >= clock.unix_timestamp, ErrorCode::InvalidDates);

        let job_post = &mut ctx.accounts.job_post;
        job_post.client = ctx.accounts.client.key();
        job_post.title = title;
        job_post.description = description;
        job_post.amount = amount;
        job_post.is_filled = false;
        job_post.start_date = start_date;
        job_post.end_date = end_date;
        job_post.escrow_bump = ctx.bumps.escrow;
        job_post.cancelled = false;
        job_post.freelancer = None;

        // Derive PDA seeds for escrow
        let job_post_key = job_post.key();
        let escrow_key = ctx.accounts.escrow.key();
        let bump = ctx.bumps.escrow;
        let seeds = &[b"escrow", job_post_key.as_ref(), &[bump]];
        let signer_seeds = &[&seeds[..]];

        // Create the escrow account (a pure system account to hold lamports)
        let lamports = Rent::get()?.minimum_balance(0).max(amount);
        invoke_signed(
            &system_instruction::create_account(
                &ctx.accounts.client.key(),
                &escrow_key,
                lamports,
                0, // 0 bytes = no data
                &system_program::ID,
            ),
            &[
                ctx.accounts.client.to_account_info(),
                ctx.accounts.escrow.to_account_info(),
                ctx.accounts.system_program.to_account_info(),
            ],
            signer_seeds,
        )?;

        // Transfer job amount from client to escrow
        let cpi_ctx = CpiContext::new(
            ctx.accounts.system_program.to_account_info(),
            system_program::Transfer {
                from: ctx.accounts.client.to_account_info(),
                to: ctx.accounts.escrow.to_account_info(),
            },
        );
        system_program::transfer(cpi_ctx, amount)?;

        // Update client stats
        let client_stats = &mut ctx.accounts.client_stats;

        // Get current month (1–12)
        let month = (Clock::get()?.unix_timestamp / 2_592_000) % 12 + 1; // ~30 days

        if client_stats.last_updated_month != month as u8 {
            client_stats.monthly_gigs = 0;
            client_stats.monthly_revenue = 0;
            client_stats.last_updated_month = month as u8;
        }

        client_stats.total_gigs_posted += 1;
        client_stats.monthly_gigs += 1;

        msg!(
            "✅ Job post created: '{}' for {} lamports. Escrow: {}",
            job_post.title,
            amount,
            escrow_key
        );

        Ok(())
    }

    // Freelancer applies to a job
    pub fn apply_to_job(
        ctx: Context<ApplyToJob>,
        resume_link: String,
        expected_end_date: i64,
    ) -> Result<()> {
        require!(!resume_link.is_empty(), ErrorCode::InvalidInput);
        require!(expected_end_date >= 0, ErrorCode::InvalidDates);
        require!(
            !ctx.accounts.job_post.is_filled,
            ErrorCode::JobAlreadyFilled
        );
        require!(!ctx.accounts.job_post.cancelled, ErrorCode::JobCancelled);

        let application = &mut ctx.accounts.application;
        application.applicant = ctx.accounts.freelancer.key();
        application.job_post = ctx.accounts.job_post.key();
        application.resume_link = resume_link;
        application.approved = false;
        application.completed = false;
        application.submission_link = String::new();
        application.narration = String::new();
        application.client_review = String::new();
        application.expected_end_date = expected_end_date;

        application.submitted = false;
        application.rejected = false;

        msg!("📩 Application submitted by {}", application.applicant);
        Ok(())
    }

    // Client approves a freelancer's application
    pub fn approve_application(ctx: Context<ApproveApplication>) -> Result<()> {
        let job_post = &mut ctx.accounts.job_post;
        let application = &mut ctx.accounts.application;

        require!(
            job_post.client == ctx.accounts.client.key(),
            ErrorCode::Unauthorized
        );
        require!(!job_post.is_filled, ErrorCode::JobAlreadyFilled);
        require!(!job_post.cancelled, ErrorCode::JobCancelled);
        require!(
            application.job_post == job_post.key(),
            ErrorCode::InvalidAccount
        );
        require!(!application.approved, ErrorCode::ApplicationAlreadyApproved);

        application.approved = true;
        job_post.is_filled = true;
        job_post.freelancer = Some(application.applicant);

        msg!("✅ Application approved for job '{}'", job_post.title);
        Ok(())
    }

    // Freelancer submits their completed work
    pub fn submit_work(
        ctx: Context<SubmitWork>,
        submission_link: String,
        narration: String,
    ) -> Result<()> {
        require!(!submission_link.is_empty(), ErrorCode::InvalidInput);
        require!(!narration.is_empty(), ErrorCode::InvalidInput);

        let application = &mut ctx.accounts.application;

        require!(
            application.applicant == ctx.accounts.freelancer.key(),
            ErrorCode::Unauthorized
        );
        require!(application.approved, ErrorCode::ApplicationNotApproved);
        require!(!application.completed, ErrorCode::WorkAlreadyApproved);

        // ✅ allow resubmission if rejected
        application.submission_link = submission_link;
        application.narration = narration;
        application.submitted = true;
        application.rejected = false; // reset rejection flag

        msg!("📤 Work submitted by {}", application.applicant);
        Ok(())
    }

    // Client approves work and releases escrow funds to freelancer
    pub fn approve_submission(
        ctx: Context<ApproveSubmission>,
        client_review: String,
    ) -> Result<()> {
        let job_post = &ctx.accounts.job_post;
        let application = &mut ctx.accounts.application;

        // --- VALIDATIONS ---
        require!(
            job_post.client == ctx.accounts.client.key(),
            ErrorCode::Unauthorized
        );
        require!(application.submitted, ErrorCode::WorkNotCompleted);
        require!(!application.completed, ErrorCode::WorkAlreadyApproved);
        require!(
            application.job_post == job_post.key(),
            ErrorCode::InvalidAccount
        );
        require!(
            job_post.freelancer == Some(application.applicant),
            ErrorCode::Unauthorized
        );

        // Ensure escrow has enough lamports
        require!(
            **ctx.accounts.escrow.to_account_info().lamports.borrow() >= job_post.amount,
            ErrorCode::InsufficientEscrowBalance
        );

        // --- UPDATE APPLICATION STATUS ---
        application.client_review = client_review;
        application.completed = true;

        // --- TRANSFER FUNDS FROM ESCROW TO FREELANCER ---
        let job_post_key = job_post.key();
        let seeds = &[b"escrow", job_post_key.as_ref(), &[job_post.escrow_bump]];
        let signer_seeds = &[&seeds[..]];

        let cpi_ctx = CpiContext::new_with_signer(
            ctx.accounts.system_program.to_account_info(),
            system_program::Transfer {
                from: ctx.accounts.escrow.to_account_info(),
                to: ctx.accounts.freelancer.to_account_info(),
            },
            signer_seeds,
        );

        system_program::transfer(cpi_ctx, job_post.amount)?;

        // --- UPDATE FREELANCER STATS ---
        let freelancer_stats = &mut ctx.accounts.freelancer_stats;
        let current_time = Clock::get()?.unix_timestamp;
        let current_month = (current_time / 2_592_000) % 12 + 1; // ~30 days per month

        if freelancer_stats.last_updated_month != current_month as u8 {
            freelancer_stats.monthly_gigs = 0;
            freelancer_stats.monthly_revenue = 0;
            freelancer_stats.last_updated_month = current_month as u8;
        }

        freelancer_stats.total_revenue_earned += job_post.amount;
        freelancer_stats.monthly_revenue += job_post.amount;
        freelancer_stats.monthly_gigs += 1;

        msg!(
            "💸 Funds released to freelancer: {} lamports. Stats updated.",
            job_post.amount
        );

        Ok(())
    }

    pub fn reject_submission(ctx: Context<RejectSubmission>, client_review: String) -> Result<()> {
        let job_post = &ctx.accounts.job_post;
        let application = &mut ctx.accounts.application;

        require!(
            job_post.client == ctx.accounts.client.key(),
            ErrorCode::Unauthorized
        );
        require!(!application.completed, ErrorCode::WorkAlreadyApproved);
        require!(application.submitted, ErrorCode::WorkNotCompleted);

        application.client_review = client_review;
        application.rejected = true;
        application.submitted = false; // Allow resubmission

        msg!("❌ Work rejected. Feedback: {}", application.client_review);
        Ok(())
    }

    // Client cancels job and gets refund (only if no freelancer approved)
    pub fn cancel_job(ctx: Context<CancelJob>) -> Result<()> {
        let job_post = &mut ctx.accounts.job_post;

        require!(
            job_post.client == ctx.accounts.client.key(),
            ErrorCode::Unauthorized
        );
        require!(!job_post.is_filled, ErrorCode::JobAlreadyFilled);
        require!(!job_post.cancelled, ErrorCode::JobAlreadyCancelled);

        job_post.cancelled = true;

        // Refund client from escrow
        let job_post_key = job_post.key();
        let seeds = &[b"escrow", job_post_key.as_ref(), &[job_post.escrow_bump]];
        let signer_seeds = &[&seeds[..]];

        let cpi_ctx = CpiContext::new_with_signer(
            ctx.accounts.system_program.to_account_info(),
            system_program::Transfer {
                from: ctx.accounts.escrow.to_account_info(),
                to: ctx.accounts.client.to_account_info(),
            },
            signer_seeds,
        );
        system_program::transfer(cpi_ctx, job_post.amount)?;

        msg!("❌ Job cancelled and funds refunded to client");
        Ok(())
    }

    // Fetch user statistics
    pub fn get_user_stats(ctx: Context<GetUserStats>) -> Result<()> {
        let stats = &ctx.accounts.user_stats;
        msg!("📊 User Stats:");
        msg!("Total Gigs Posted: {}", stats.total_gigs_posted);
        msg!("Total Revenue Earned: {}", stats.total_revenue_earned);
        msg!("Monthly Gigs: {}", stats.monthly_gigs);
        msg!("Monthly Revenue: {}", stats.monthly_revenue);
        msg!("Last Updated Month: {}", stats.last_updated_month);
        Ok(())
    }
}

// ----------------- ACCOUNTS -----------------

#[account]
#[derive(InitSpace)]
pub struct JobPost {
    pub client: Pubkey,
    #[max_len(100)]
    pub title: String,
    #[max_len(500)]
    pub description: String,
    pub amount: u64,
    pub is_filled: bool,
    pub cancelled: bool,
    pub start_date: i64,
    pub end_date: i64,
    pub escrow_bump: u8,
    pub freelancer: Option<Pubkey>,
}

#[account]
#[derive(InitSpace)]
pub struct Application {
    pub applicant: Pubkey,
    pub job_post: Pubkey,
    #[max_len(200)]
    pub resume_link: String,
    #[max_len(200)]
    pub submission_link: String,
    #[max_len(300)]
    pub narration: String,
    #[max_len(300)]
    pub client_review: String,
    pub approved: bool,
    pub submitted: bool,
    pub completed: bool,
    pub rejected: bool,
    pub expected_end_date: i64,
}

#[account]
#[derive(InitSpace)]
pub struct UserStats {
    pub total_gigs_posted: u64,
    pub total_revenue_earned: u64,
    pub monthly_gigs: u64,
    pub monthly_revenue: u64,
    pub last_updated_month: u8,
}

// ----------------- CONTEXTS -----------------

#[derive(Accounts)]
#[instruction(title: String)]
pub struct InitializeJobPost<'info> {
    #[account(
        init,
        payer = client,
        space = 8 + JobPost::INIT_SPACE,
        seeds = [b"job_post", client.key().as_ref(), title.as_bytes()],
        bump
    )]
    pub job_post: Account<'info, JobPost>,

    #[account(
        mut,
        seeds = [b"escrow", job_post.key().as_ref()],
        bump
    )]
    /// CHECK: Escrow PDA (pure lamport vault, no data)
    pub escrow: UncheckedAccount<'info>,

    #[account(
        init_if_needed,
        payer = client,
        space = 8 + UserStats::INIT_SPACE,
        seeds = [b"user_stats", client.key().as_ref()],
        bump
    )]
    pub client_stats: Account<'info, UserStats>,

    #[account(mut)]
    pub client: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct ApplyToJob<'info> {
    #[account(
        init,
        payer = freelancer,
        space = 8 + Application::INIT_SPACE,
        seeds = [b"application", job_post.key().as_ref(), freelancer.key().as_ref()],
        bump
    )]
    pub application: Account<'info, Application>,

    #[account(mut)]
    pub freelancer: Signer<'info>,
    pub job_post: Account<'info, JobPost>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct ApproveApplication<'info> {
    #[account(
        mut,
        constraint = application.job_post == job_post.key() @ ErrorCode::InvalidAccount
    )]
    pub application: Account<'info, Application>,

    #[account(
        mut,
        constraint = job_post.client == client.key() @ ErrorCode::Unauthorized
    )]
    pub job_post: Account<'info, JobPost>,

    #[account(mut)]
    pub client: Signer<'info>,
}

#[derive(Accounts)]
pub struct SubmitWork<'info> {
    #[account(
        mut,
        constraint = application.applicant == freelancer.key() @ ErrorCode::Unauthorized,
        constraint = application.job_post == job_post.key() @ ErrorCode::InvalidAccount
    )]
    pub application: Account<'info, Application>,

    #[account(mut)]
    pub freelancer: Signer<'info>,

    pub job_post: Account<'info, JobPost>,
}

#[derive(Accounts)]
pub struct ApproveSubmission<'info> {
    #[account(
        mut,
        constraint = application.job_post == job_post.key() @ ErrorCode::InvalidAccount
    )]
    pub application: Account<'info, Application>,

    #[account(
        mut,
        constraint = job_post.client == client.key() @ ErrorCode::Unauthorized
    )]
    pub job_post: Account<'info, JobPost>,

    #[account(
        mut,
        seeds = [b"escrow", job_post.key().as_ref()],
        bump = job_post.escrow_bump
    )]
    /// CHECK: Escrow PDA (pure lamport vault)
    pub escrow: UncheckedAccount<'info>,

    #[account(mut)]
    pub client: Signer<'info>,

    #[account(mut)]
    /// CHECK: Freelancer wallet
    pub freelancer: UncheckedAccount<'info>,

    #[account(
        init_if_needed,
        payer = client,
        space = 8 + UserStats::INIT_SPACE,
        seeds = [b"user_stats", freelancer.key().as_ref()],
        bump
    )]
    pub freelancer_stats: Account<'info, UserStats>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct CancelJob<'info> {
    #[account(
        mut,
        constraint = job_post.client == client.key() @ ErrorCode::Unauthorized
    )]
    pub job_post: Account<'info, JobPost>,

    #[account(
        mut,
        seeds = [b"escrow", job_post.key().as_ref()],
        bump = job_post.escrow_bump
    )]
    /// CHECK: Escrow account
    pub escrow: UncheckedAccount<'info>,

    #[account(mut)]
    pub client: Signer<'info>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct RejectSubmission<'info> {
    #[account(
        mut,
        constraint = application.job_post == job_post.key() @ ErrorCode::InvalidAccount
    )]
    pub application: Account<'info, Application>,

    #[account(
        mut,
        constraint = job_post.client == client.key() @ ErrorCode::Unauthorized,
        constraint = job_post.freelancer == Some(application.applicant) @ ErrorCode::Unauthorized 
    )]
    pub job_post: Account<'info, JobPost>,

    #[account(mut)]
    pub client: Signer<'info>,
}

#[derive(Accounts)]
pub struct GetUserStats<'info> {
    #[account(
        seeds = [b"user_stats", user.key().as_ref()],
        bump
    )]
    pub user_stats: Account<'info, UserStats>,

    /// CHECK: The user whose stats we're querying
    pub user: UncheckedAccount<'info>,
}

// ----------------- ERRORS -----------------

#[error_code]
pub enum ErrorCode {
    #[msg("You are not authorized to perform this action.")]
    Unauthorized,
    #[msg("This job has already been filled.")]
    JobAlreadyFilled,
    #[msg("Application has not been approved yet.")]
    ApplicationNotApproved,
    #[msg("Work has not been completed yet.")]
    WorkNotCompleted,
    #[msg("Invalid dates provided.")]
    InvalidDates,
    #[msg("Invalid input provided.")]
    InvalidInput,
    #[msg("Invalid account relationship.")]
    InvalidAccount,
    #[msg("Invalid amount provided.")]
    InvalidAmount,
    #[msg("Job has been cancelled.")]
    JobCancelled,
    #[msg("Job has already been cancelled.")]
    JobAlreadyCancelled,
    #[msg("Work has already been submitted.")]
    WorkAlreadySubmitted,
    #[msg("Application has already been approved.")]
    ApplicationAlreadyApproved,
    #[msg("Work has already been approved.")]
    WorkAlreadyApproved,
    #[msg("Work has already been rejected.")]
    WorkAlreadyRejected,
    #[msg("Escrow account does not have enough balance.")]
    InsufficientEscrowBalance,
}