use anchor_lang::prelude::*;

declare_id!("6GQdMpD72WwzxsXm6q52ED3UDYFU4i7tH8pLVoHgSBcY");

const MAX_DOCTORS: usize = 5;
const APPROVAL_WINDOW_SECS: i64 = 86_400; // 24 hours

#[program]
pub mod surgi_consent {
    use super::*;

    /// One-time setup. Callable by anyone; the PDA init constraint enforces it runs only once.
    pub fn initialize_config(
        ctx: Context<InitializeConfig>,
        hospital: Pubkey,
    ) -> Result<()> {
        require!(hospital != Pubkey::default(), AuditError::InvalidInput);
        let config = &mut ctx.accounts.config;
        config.hospital = hospital;
        config.whitelisted_doctors = Vec::new();
        config.bump = ctx.bumps.config;
        Ok(())
    }

    /// Add or remove a doctor from the whitelist. Hospital-only. Max 5 doctors.
    pub fn manage_doctor(
        ctx: Context<ManageDoctor>,
        doctor: Pubkey,
        add: bool,
    ) -> Result<()> {
        require!(doctor != Pubkey::default(), AuditError::InvalidInput);
        require!(doctor != ctx.accounts.hospital.key(), AuditError::Unauthorized);
        let config = &mut ctx.accounts.config;
        if add {
            require!(
                !config.whitelisted_doctors.contains(&doctor),
                AuditError::DoctorAlreadyWhitelisted
            );
            require!(
                config.whitelisted_doctors.len() < MAX_DOCTORS,
                AuditError::DoctorLimitReached
            );
            config.whitelisted_doctors.push(doctor);
        } else {
            let pos = config
                .whitelisted_doctors
                .iter()
                .position(|d| d == &doctor);
            require!(pos.is_some(), AuditError::DoctorNotFound);
            config.whitelisted_doctors.swap_remove(pos.unwrap());
        }

        emit!(DoctorManaged {
            doctor,
            added: add,
            hospital: ctx.accounts.hospital.key(),
            timestamp: Clock::get()?.unix_timestamp,
        });

        Ok(())
    }

    /// Registers a patient. Hospital-only.
    /// No PHI is stored on-chain: only a hash, a minor flag, and an optional surrogate.
    /// If is_minor is true a surrogate address must be provided.
    pub fn create_patient(
        ctx: Context<CreatePatient>,
        patient_hash: [u8; 32],
        is_minor: bool,
        surrogate: Option<Pubkey>,
    ) -> Result<()> {
        if is_minor {
            require!(surrogate.is_some(), AuditError::SurrogateRequired);
            require!(
                surrogate != Some(Pubkey::default()),
                AuditError::InvalidInput
            );
        }

        let patient_wallet = ctx.accounts.patient_wallet.key();
        let acct = &mut ctx.accounts.patient_account;
        acct.patient = patient_wallet;
        acct.patient_hash = patient_hash;
        acct.is_minor = is_minor;
        acct.surrogate = surrogate;
        acct.hospital = ctx.accounts.config.hospital;
        acct.bump = ctx.bumps.patient_account;

        emit!(PatientCreated {
            patient: patient_wallet,
            patient_hash,
            is_minor,
            hospital: acct.hospital,
        });

        Ok(())
    }

    /// Creates a new consent record with status Pending.
    /// Signer must be the hospital or a whitelisted doctor.
    pub fn create_consent(
        ctx: Context<CreateConsent>,
        consent_id: u64,
        procedure_template_id: u64,
        document_hash: [u8; 32],
    ) -> Result<()> {
        let signer_key = ctx.accounts.signer.key();
        let config = &ctx.accounts.config;
        let is_hospital = signer_key == config.hospital;
        let is_doctor = config.whitelisted_doctors.contains(&signer_key);
        require!(is_hospital || is_doctor, AuditError::NotHospitalOrDoctor);
        require!(document_hash != [0u8; 32], AuditError::InvalidInput);

        let patient_wallet = ctx.accounts.patient_wallet.key();
        let acct = &mut ctx.accounts.consent;
        acct.consent_id = consent_id;
        acct.procedure_template_id = procedure_template_id;
        acct.document_hash = document_hash;
        acct.status = ConsentStatus::Pending;
        acct.created_by = signer_key;
        acct.signed_by = Pubkey::default();
        acct.signed_at = 0;
        acct.approval_expires_at = 0;
        acct.timestamp = Clock::get()?.unix_timestamp;
        acct.version = 1;
        acct.patient = patient_wallet;
        acct.hospital = config.hospital;
        acct.bump = ctx.bumps.consent;

        emit!(ConsentCreated {
            consent_id,
            procedure_template_id,
            document_hash,
            status: ConsentStatus::Pending,
            created_by: signer_key,
            timestamp: acct.timestamp,
            version: acct.version,
            patient: patient_wallet,
            hospital: acct.hospital,
        });

        Ok(())
    }

    /// Patient or surrogate signs the consent.
    /// Transitions status Pending → Signed and opens the 24-hour approval window.
    pub fn sign_consent(ctx: Context<SignConsent>) -> Result<()> {
        let patient = &ctx.accounts.patient_account;
        let signer_key = ctx.accounts.signer.key();

        validate_transition(ctx.accounts.consent.status, ConsentStatus::Signed)?;

        if patient.is_minor {
            let surrogate = patient.surrogate.ok_or(error!(AuditError::SurrogateRequired))?;
            require!(signer_key == surrogate, AuditError::Unauthorized);
        } else {
            require!(
                signer_key == ctx.accounts.consent.patient,
                AuditError::Unauthorized
            );
        }

        let now = Clock::get()?.unix_timestamp;
        let acct = &mut ctx.accounts.consent;
        acct.status = ConsentStatus::Signed;
        acct.signed_by = signer_key;
        acct.signed_at = now;
        acct.approval_expires_at = now + APPROVAL_WINDOW_SECS;
        acct.timestamp = now;
        acct.version = acct.version.saturating_add(1);

        emit!(PatientSigned {
            consent_id: acct.consent_id,
            patient: acct.patient,
            signed_by: signer_key,
            timestamp: now,
            version: acct.version,
            hospital: acct.hospital,
        });

        Ok(())
    }

    /// Whitelisted doctor approves the consent within the 24-hour window.
    /// Transitions status Signed → Approved.
    pub fn approve_consent(ctx: Context<ApproveConsent>) -> Result<()> {
        let doctor_key = ctx.accounts.approver.key();
        require!(
            ctx.accounts.config.whitelisted_doctors.contains(&doctor_key),
            AuditError::NotWhitelistedDoctor
        );
        require!(
            doctor_key != ctx.accounts.config.hospital,
            AuditError::Unauthorized
        );

        // All read-only guards before taking the mutable borrow.
        validate_transition(ctx.accounts.consent.status, ConsentStatus::Approved)?;
        require_not_expired(&ctx.accounts.consent)?;

        let now = Clock::get()?.unix_timestamp;
        let acct = &mut ctx.accounts.consent;
        acct.status = ConsentStatus::Approved;
        acct.timestamp = now;
        acct.version = acct.version.saturating_add(1);

        emit!(ConsentApproved {
            consent_id: acct.consent_id,
            patient: acct.patient,
            approved_by: doctor_key,
            timestamp: now,
            version: acct.version,
            hospital: acct.hospital,
        });

        Ok(())
    }

    /// Whitelisted doctor rejects the consent within the 24-hour window.
    /// Transitions status Signed → Rejected.
    pub fn reject_consent(ctx: Context<RejectConsent>) -> Result<()> {
        let doctor_key = ctx.accounts.approver.key();
        require!(
            ctx.accounts.config.whitelisted_doctors.contains(&doctor_key),
            AuditError::NotWhitelistedDoctor
        );
        require!(
            doctor_key != ctx.accounts.config.hospital,
            AuditError::Unauthorized
        );

        validate_transition(ctx.accounts.consent.status, ConsentStatus::Rejected)?;
        require_not_expired(&ctx.accounts.consent)?;

        let now = Clock::get()?.unix_timestamp;
        let acct = &mut ctx.accounts.consent;
        acct.status = ConsentStatus::Rejected;
        acct.timestamp = now;
        acct.version = acct.version.saturating_add(1);

        emit!(ConsentRejected {
            consent_id: acct.consent_id,
            patient: acct.patient,
            rejected_by: doctor_key,
            timestamp: now,
            version: acct.version,
            hospital: acct.hospital,
        });

        Ok(())
    }

    /// Marks the consent as Expired. Permissionless — anyone can call this once the
    /// 24-hour window has passed without a doctor decision. Solana has no timers;
    /// expiry must be triggered explicitly.
    pub fn expire_consent(ctx: Context<ExpireConsent>) -> Result<()> {
        // validate_transition enforces Signed → Expired; any other from-state errors.
        validate_transition(ctx.accounts.consent.status, ConsentStatus::Expired)?;

        let now = Clock::get()?.unix_timestamp;
        require!(
            now >= ctx.accounts.consent.approval_expires_at,
            AuditError::ApprovalWindowStillOpen
        );

        let acct = &mut ctx.accounts.consent;
        acct.status = ConsentStatus::Expired;
        acct.timestamp = now;
        acct.version = acct.version.saturating_add(1);

        emit!(ConsentExpired {
            consent_id: acct.consent_id,
            patient: acct.patient,
            timestamp: now,
            version: acct.version,
            hospital: acct.hospital,
        });

        Ok(())
    }

    /// Updates the document hash to reflect a change in consent terms. Doctor-only.
    /// Blocked once the patient has signed — the document is immutable at that point.
    pub fn update_document_hash(
        ctx: Context<UpdateDocumentHash>,
        document_hash: [u8; 32],
    ) -> Result<()> {
        let doctor_key = ctx.accounts.doctor.key();
        require!(
            ctx.accounts.config.whitelisted_doctors.contains(&doctor_key),
            AuditError::NotWhitelistedDoctor
        );

        // Pending means the patient has not yet signed; once status advances beyond
        // Pending the document is immutable.
        require!(
            ctx.accounts.consent.status == ConsentStatus::Pending,
            AuditError::DocumentLocked
        );

        let acct = &mut ctx.accounts.consent;
        acct.document_hash = document_hash;
        acct.timestamp = Clock::get()?.unix_timestamp;
        acct.version = acct.version.saturating_add(1);

        emit!(DocumentHashUpdated {
            consent_id: acct.consent_id,
            patient: acct.patient,
            document_hash: acct.document_hash,
            status: acct.status,
            timestamp: acct.timestamp,
            version: acct.version,
            hospital: acct.hospital,
        });

        Ok(())
    }

    /// Withdraws a consent. Hospital-only.
    /// Valid from Pending, Signed, or Approved.
    /// If status is Signed, the 24-hour window must still be open (call expire_consent first otherwise).
    pub fn withdraw_consent(ctx: Context<WithdrawConsent>) -> Result<()> {
        let current = ctx.accounts.consent.status;
        validate_transition(current, ConsentStatus::Withdrawn)?;

        // Prevent bypassing expiry: a Signed consent past its window must be formally
        // expired before it can be withdrawn.
        if current == ConsentStatus::Signed {
            require_not_expired(&ctx.accounts.consent)?;
        }

        let acct = &mut ctx.accounts.consent;
        acct.status = ConsentStatus::Withdrawn;
        acct.timestamp = Clock::get()?.unix_timestamp;
        acct.version = acct.version.saturating_add(1);

        emit!(ConsentWithdrawn {
            consent_id: acct.consent_id,
            patient: acct.patient,
            document_hash: acct.document_hash,
            status: acct.status,
            timestamp: acct.timestamp,
            version: acct.version,
            hospital: acct.hospital,
        });

        Ok(())
    }

    /// Forces the consent to Overridden from any state. Hospital-only.
    pub fn emergency_override(
        ctx: Context<EmergencyOverride>,
        reason_hash: [u8; 32],
        document_hash: [u8; 32],
    ) -> Result<()> {
        require!(reason_hash != [0u8; 32], AuditError::InvalidInput);
        // emergency_override intentionally bypasses the normal state machine.
        let acct = &mut ctx.accounts.consent;
        acct.status = ConsentStatus::Overridden;
        acct.document_hash = document_hash;
        acct.timestamp = Clock::get()?.unix_timestamp;
        acct.version = acct.version.saturating_add(1);

        emit!(EmergencyOverrideEvent {
            consent_id: acct.consent_id,
            patient: acct.patient,
            reason_hash,
            document_hash: acct.document_hash,
            status: acct.status,
            timestamp: acct.timestamp,
            version: acct.version,
            hospital: acct.hospital,
        });

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// State machine helpers
// ---------------------------------------------------------------------------

/// Central transition table — the single source of truth for all status changes.
/// Every status-changing instruction calls this before writing to the account,
/// except emergency_override which explicitly bypasses the normal state machine.
///
/// Allowed paths:
///   Pending  → Signed                 sign_consent     (patient or surrogate)
///   Signed   → Approved               approve_consent  (doctor, within 24 h)
///   Signed   → Rejected               reject_consent   (doctor, within 24 h)
///   Signed   → Expired                expire_consent   (permissionless, after 24 h)
///   Pending  → Withdrawn              withdraw_consent (hospital)
///   Signed   → Withdrawn              withdraw_consent (hospital, window still open)
///   Approved → Withdrawn              withdraw_consent (hospital)
fn validate_transition(from: ConsentStatus, to: ConsentStatus) -> Result<()> {
    let allowed = matches!(
        (from, to),
        (ConsentStatus::Pending,  ConsentStatus::Signed)    |
        (ConsentStatus::Signed,   ConsentStatus::Approved)  |
        (ConsentStatus::Signed,   ConsentStatus::Rejected)  |
        (ConsentStatus::Signed,   ConsentStatus::Expired)   |
        (ConsentStatus::Pending,  ConsentStatus::Withdrawn) |
        (ConsentStatus::Signed,   ConsentStatus::Withdrawn) |
        (ConsentStatus::Approved, ConsentStatus::Withdrawn)
    );
    require!(allowed, AuditError::InvalidTransition);
    Ok(())
}

/// Checks that the 24-hour approval window is still open for a Signed consent.
/// Call this before any instruction that must act within the window (approve, reject, withdraw).
fn require_not_expired(consent: &ConsentAccount) -> Result<()> {
    if consent.status == ConsentStatus::Signed {
        let now = Clock::get()?.unix_timestamp;
        require!(
            now < consent.approval_expires_at,
            AuditError::ConsentWindowExpired
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Account contexts
// ---------------------------------------------------------------------------

#[derive(Accounts)]
pub struct InitializeConfig<'info> {
    #[account(
        init,
        payer = payer,
        space = ConsentConfig::SPACE,
        seeds = [b"consent_config"],
        bump,
    )]
    pub config: Account<'info, ConsentConfig>,

    #[account(mut)]
    pub payer: Signer<'info>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct ManageDoctor<'info> {
    #[account(
        mut,
        seeds = [b"consent_config"],
        bump = config.bump,
        has_one = hospital @ AuditError::Unauthorized,
    )]
    pub config: Account<'info, ConsentConfig>,

    pub hospital: Signer<'info>,
}

#[derive(Accounts)]
pub struct CreatePatient<'info> {
    #[account(
        init,
        payer = hospital,
        space = PatientAccount::SPACE,
        seeds = [b"patient", patient_wallet.key().as_ref()],
        bump,
    )]
    pub patient_account: Account<'info, PatientAccount>,

    /// CHECK: used only as a PDA seed to bind this record to a specific wallet.
    pub patient_wallet: UncheckedAccount<'info>,

    #[account(
        seeds = [b"consent_config"],
        bump = config.bump,
        has_one = hospital @ AuditError::Unauthorized,
    )]
    pub config: Account<'info, ConsentConfig>,

    #[account(mut)]
    pub hospital: Signer<'info>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(consent_id: u64)]
pub struct CreateConsent<'info> {
    #[account(
        init,
        payer = signer,
        space = ConsentAccount::SPACE,
        seeds = [
            b"consent",
            patient_wallet.key().as_ref(),
            &consent_id.to_le_bytes(),
        ],
        bump,
    )]
    pub consent: Account<'info, ConsentAccount>,

    /// CHECK: used only as a PDA seed — no PHI.
    pub patient_wallet: UncheckedAccount<'info>,

    /// Patient must be registered before a consent can be created for them.
    #[account(
        seeds = [b"patient", patient_wallet.key().as_ref()],
        bump = patient_account.bump,
    )]
    pub patient_account: Account<'info, PatientAccount>,

    #[account(
        seeds = [b"consent_config"],
        bump = config.bump,
    )]
    pub config: Account<'info, ConsentConfig>,

    /// Must be the hospital or a whitelisted doctor.
    #[account(mut)]
    pub signer: Signer<'info>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct SignConsent<'info> {
    #[account(
        mut,
        seeds = [
            b"consent",
            patient_wallet.key().as_ref(),
            &consent.consent_id.to_le_bytes(),
        ],
        bump = consent.bump,
    )]
    pub consent: Account<'info, ConsentAccount>,

    #[account(
        seeds = [b"patient", patient_wallet.key().as_ref()],
        bump = patient_account.bump,
    )]
    pub patient_account: Account<'info, PatientAccount>,

    /// CHECK: used only as a PDA seed — patient identity is verified against the stored consent.patient field.
    pub patient_wallet: UncheckedAccount<'info>,

    /// Patient (not a minor) or registered surrogate (minor).
    pub signer: Signer<'info>,
}

#[derive(Accounts)]
pub struct ApproveConsent<'info> {
    #[account(
        mut,
        seeds = [
            b"consent",
            patient_wallet.key().as_ref(),
            &consent.consent_id.to_le_bytes(),
        ],
        bump = consent.bump,
    )]
    pub consent: Account<'info, ConsentAccount>,

    /// CHECK: used only as a PDA seed.
    pub patient_wallet: UncheckedAccount<'info>,

    #[account(
        seeds = [b"consent_config"],
        bump = config.bump,
    )]
    pub config: Account<'info, ConsentConfig>,

    /// Must be a whitelisted doctor.
    pub approver: Signer<'info>,
}

#[derive(Accounts)]
pub struct RejectConsent<'info> {
    #[account(
        mut,
        seeds = [
            b"consent",
            patient_wallet.key().as_ref(),
            &consent.consent_id.to_le_bytes(),
        ],
        bump = consent.bump,
    )]
    pub consent: Account<'info, ConsentAccount>,

    /// CHECK: used only as a PDA seed.
    pub patient_wallet: UncheckedAccount<'info>,

    #[account(
        seeds = [b"consent_config"],
        bump = config.bump,
    )]
    pub config: Account<'info, ConsentConfig>,

    /// Must be a whitelisted doctor.
    pub approver: Signer<'info>,
}

#[derive(Accounts)]
pub struct ExpireConsent<'info> {
    #[account(
        mut,
        seeds = [
            b"consent",
            patient_wallet.key().as_ref(),
            &consent.consent_id.to_le_bytes(),
        ],
        bump = consent.bump,
    )]
    pub consent: Account<'info, ConsentAccount>,

    /// CHECK: used only as a PDA seed.
    pub patient_wallet: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct UpdateDocumentHash<'info> {
    #[account(
        mut,
        seeds = [
            b"consent",
            patient_wallet.key().as_ref(),
            &consent.consent_id.to_le_bytes(),
        ],
        bump = consent.bump,
    )]
    pub consent: Account<'info, ConsentAccount>,

    /// CHECK: used only as a PDA seed.
    pub patient_wallet: UncheckedAccount<'info>,

    #[account(
        seeds = [b"consent_config"],
        bump = config.bump,
    )]
    pub config: Account<'info, ConsentConfig>,

    /// Must be a whitelisted doctor.
    pub doctor: Signer<'info>,
}

#[derive(Accounts)]
pub struct WithdrawConsent<'info> {
    #[account(
        mut,
        seeds = [
            b"consent",
            patient_wallet.key().as_ref(),
            &consent.consent_id.to_le_bytes(),
        ],
        bump = consent.bump,
        has_one = hospital @ AuditError::Unauthorized,
    )]
    pub consent: Account<'info, ConsentAccount>,

    /// CHECK: used only as a PDA seed.
    pub patient_wallet: UncheckedAccount<'info>,

    pub hospital: Signer<'info>,
}

#[derive(Accounts)]
pub struct EmergencyOverride<'info> {
    #[account(
        mut,
        seeds = [
            b"consent",
            patient_wallet.key().as_ref(),
            &consent.consent_id.to_le_bytes(),
        ],
        bump = consent.bump,
        has_one = hospital @ AuditError::Unauthorized,
    )]
    pub consent: Account<'info, ConsentAccount>,

    /// CHECK: used only as a PDA seed.
    pub patient_wallet: UncheckedAccount<'info>,

    pub hospital: Signer<'info>,
}

// ---------------------------------------------------------------------------
// Account structs
// ---------------------------------------------------------------------------

#[account]
pub struct ConsentConfig {
    pub hospital: Pubkey,
    pub whitelisted_doctors: Vec<Pubkey>,
    pub bump: u8,
}

impl ConsentConfig {
    pub const SPACE: usize =
        8                       // discriminator
        + 32                    // hospital
        + 4 + MAX_DOCTORS * 32  // Vec<Pubkey> capped at MAX_DOCTORS
        + 1;                    // bump
}

/// Stores the minimum on-chain data needed to enforce consent rules.
/// No PHI (name, age, gender) is stored — only a hash, a minor flag, and an optional surrogate.
#[account]
pub struct PatientAccount {
    pub patient: Pubkey,
    pub patient_hash: [u8; 32],
    /// True if the patient was under 18 at registration time; drives surrogate signing rules.
    pub is_minor: bool,
    pub surrogate: Option<Pubkey>,
    pub hospital: Pubkey,
    pub bump: u8,
}

impl PatientAccount {
    pub const SPACE: usize =
        8           // discriminator
        + 32        // patient wallet
        + 32        // patient_hash
        + 1         // is_minor
        + 1 + 32    // Option<Pubkey> surrogate
        + 32        // hospital
        + 1;        // bump  → 139 bytes total
}

#[account]
pub struct ConsentAccount {
    pub consent_id: u64,
    pub procedure_template_id: u64,
    pub document_hash: [u8; 32],
    pub status: ConsentStatus,
    /// Wallet that created the consent (hospital or whitelisted doctor).
    pub created_by: Pubkey,
    /// Patient or surrogate who signed; zero until sign_consent is called.
    pub signed_by: Pubkey,
    /// Unix timestamp when the patient signed.
    pub signed_at: i64,
    /// Deadline for doctor approval: signed_at + 86 400 s.
    pub approval_expires_at: i64,
    pub timestamp: i64,
    pub version: u64,
    pub patient: Pubkey,
    pub hospital: Pubkey,
    pub bump: u8,
}

impl ConsentAccount {
    pub const SPACE: usize =
        8       // discriminator
        + 8     // consent_id
        + 8     // procedure_template_id
        + 32    // document_hash
        + 1     // status
        + 32    // created_by
        + 32    // signed_by
        + 8     // signed_at
        + 8     // approval_expires_at
        + 8     // timestamp
        + 8     // version
        + 32    // patient
        + 32    // hospital
        + 1;    // bump  → 218 bytes total
}

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ConsentStatus {
    Pending = 0,    // created — awaiting patient signature
    Signed = 1,     // patient signed — 24-hour approval window open
    Approved = 2,   // doctor approved within the window
    Rejected = 3,   // doctor rejected within the window
    Expired = 4,    // window closed without a doctor decision
    Withdrawn = 5,  // hospital cancelled
    Overridden = 6, // emergency override
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

#[event]
pub struct DoctorManaged {
    pub doctor: Pubkey,
    /// true = added to whitelist, false = removed
    pub added: bool,
    pub hospital: Pubkey,
    pub timestamp: i64,
}

#[event]
pub struct PatientCreated {
    pub patient: Pubkey,
    pub patient_hash: [u8; 32],
    pub is_minor: bool,
    pub hospital: Pubkey,
}

#[event]
pub struct ConsentCreated {
    pub consent_id: u64,
    pub procedure_template_id: u64,
    pub document_hash: [u8; 32],
    pub status: ConsentStatus,
    pub created_by: Pubkey,
    pub timestamp: i64,
    pub version: u64,
    pub patient: Pubkey,
    pub hospital: Pubkey,
}

/// Emitted when the patient or surrogate signs (Pending → Signed). Opens the 24-hour window.
#[event]
pub struct PatientSigned {
    pub consent_id: u64,
    pub patient: Pubkey,
    pub signed_by: Pubkey,
    pub timestamp: i64,
    pub version: u64,
    pub hospital: Pubkey,
}

/// Emitted when the doctor approves within the 24-hour window (Signed → Approved).
#[event]
pub struct ConsentApproved {
    pub consent_id: u64,
    pub patient: Pubkey,
    pub approved_by: Pubkey,
    pub timestamp: i64,
    pub version: u64,
    pub hospital: Pubkey,
}

/// Emitted when the doctor rejects within the 24-hour window (Signed → Rejected).
#[event]
pub struct ConsentRejected {
    pub consent_id: u64,
    pub patient: Pubkey,
    pub rejected_by: Pubkey,
    pub timestamp: i64,
    pub version: u64,
    pub hospital: Pubkey,
}

#[event]
pub struct ConsentExpired {
    pub consent_id: u64,
    pub patient: Pubkey,
    pub timestamp: i64,
    pub version: u64,
    pub hospital: Pubkey,
}

#[event]
pub struct DocumentHashUpdated {
    pub consent_id: u64,
    pub patient: Pubkey,
    pub document_hash: [u8; 32],
    pub status: ConsentStatus,
    pub timestamp: i64,
    pub version: u64,
    pub hospital: Pubkey,
}

#[event]
pub struct ConsentWithdrawn {
    pub consent_id: u64,
    pub patient: Pubkey,
    pub document_hash: [u8; 32],
    pub status: ConsentStatus,
    pub timestamp: i64,
    pub version: u64,
    pub hospital: Pubkey,
}

#[event]
pub struct EmergencyOverrideEvent {
    pub consent_id: u64,
    pub patient: Pubkey,
    pub reason_hash: [u8; 32],
    pub document_hash: [u8; 32],
    pub status: ConsentStatus,
    pub timestamp: i64,
    pub version: u64,
    pub hospital: Pubkey,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[error_code]
pub enum AuditError {
    #[msg("Invalid status transition")]
    InvalidTransition,
    #[msg("Unauthorized")]
    Unauthorized,
    #[msg("Invalid input: zero address or empty value")]
    InvalidInput,
    #[msg("Signer is not the hospital or a whitelisted doctor")]
    NotHospitalOrDoctor,
    #[msg("Signer is not a whitelisted doctor")]
    NotWhitelistedDoctor,
    #[msg("Doctor is already whitelisted")]
    DoctorAlreadyWhitelisted,
    #[msg("Doctor not found in whitelist")]
    DoctorNotFound,
    #[msg("Whitelist is full (max 5 doctors)")]
    DoctorLimitReached,
    #[msg("Document hash is locked after the patient has signed")]
    DocumentLocked,
    #[msg("A surrogate is required for minor patients")]
    SurrogateRequired,
    #[msg("The 24-hour approval window has expired — call expire_consent")]
    ConsentWindowExpired,
    #[msg("The 24-hour approval window is still open")]
    ApprovalWindowStillOpen,
}
