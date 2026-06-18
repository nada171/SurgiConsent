# SurgiConsent — On-Chain Audit Program

A Solana smart contract that provides a tamper-evident, immutable audit trail for surgical consent workflows. Built with [Anchor](https://www.anchor-lang.com/) 0.31.1.

> **Privacy by design.** No patient health information (PHI) is ever stored on-chain. The program records only cryptographic hashes, status transitions, and timestamps, making it a compliance and integrity layer rather than a data store.

---

## Overview

SurgiConsent anchors the consent lifecycle to the blockchain. Each consent document managed off-chain is represented by a PDA that tracks its status, document hash, and the full history of updates through on-chain events. Any tampering with a consent document is immediately detectable by comparing its SHA-256 hash against the value recorded on-chain.

The program enforces:
- **Access control** — only the registered hospital or its whitelisted doctors can create consents; only the hospital can update or override them.
- **State machine integrity** — consent status transitions are validated on-chain and cannot be bypassed.
- **Emergency access** — a dedicated override instruction allows the hospital to act in emergencies while still leaving a verifiable on-chain record.

---

## Program ID

```
6GQdMpD72WwzxsXm6q52ED3UDYFU4i7tH8pLVoHgSBcY
```

---

## Architecture

```
ConsentConfig (singleton PDA)
└── hospital: Pubkey
└── whitelisted_doctors: Vec<Pubkey>   ← max 5

PatientAccount (one per patient, hospital-created)
└── name, age, gender, patient_hash
└── surrogate: Option<Pubkey>          ← required if age < 18
└── hospital, bump

ConsentAccount (one per consent document)
└── consent_id, procedure_template_id
└── document_hash, status
└── timestamp, version, hospital
```

The `ConsentConfig` is a singleton PDA initialized once by any party. It defines the hospital authority and the set of doctors authorized to interact with the protocol. All `ConsentAccount` and `PatientAccount` PDAs derive their `hospital` field from this config at creation time, ensuring authority is always traceable back to the registered hospital.

---

## Accounts

### `ConsentConfig`

**PDA seeds:** `["consent_config"]`

Protocol-level configuration. Initialized once; the `init` constraint prevents re-initialization.

| Field | Type | Description |
|---|---|---|
| `hospital` | `Pubkey` | Hospital wallet — the ultimate authority over all consents |
| `whitelisted_doctors` | `Vec<Pubkey>` | Doctors authorized to create consents (max 5) |
| `bump` | `u8` | PDA bump seed |

### `PatientAccount`

**PDA seeds:** `["patient", patient_wallet]`

One account per patient. Contains PHI — access must be restricted off-chain.

| Field | Type | Description |
|---|---|---|
| `patient` | `Pubkey` | Patient wallet — stored explicitly so accounts are self-describing |
| `name` | `String` | Patient full name |
| `age` | `u8` | Patient age |
| `gender` | `Gender` | `Male`, `Female`, or `Other` |
| `patient_hash` | `[u8; 32]` | SHA-256 of a patient identifier for off-chain linking |
| `surrogate` | `Option<Pubkey>` | Surrogate wallet; required when age < 18 |
| `hospital` | `Pubkey` | Hospital that registered the patient |
| `bump` | `u8` | PDA bump seed |

### `ConsentAccount`

**PDA seeds:** `["consent", patient_wallet, consent_id.to_le_bytes()]`

One account per consent document. `consent_id` is a `u64` assigned by the off-chain system, encoded as 8 little-endian bytes — no hashing required.

| Field | Type | Description |
|---|---|---|
| `consent_id` | `u64` | Numeric identifier for the consent document |
| `procedure_template_id` | `u64` | Numeric procedure template identifier |
| `document_hash` | `[u8; 32]` | SHA-256 of the consent document |
| `status` | `ConsentStatus` | Current lifecycle state |
| `patient_signed` | `bool` | True once the patient or surrogate has signed; starts the 24-hour window |
| `signed_at` | `i64` | Unix timestamp of the patient signature |
| `timestamp` | `i64` | Unix timestamp of the last update |
| `version` | `u64` | Monotonically incrementing update counter |
| `patient` | `Pubkey` | Patient wallet — stored explicitly so accounts are self-describing |
| `hospital` | `Pubkey` | Hospital address at the time of creation |
| `bump` | `u8` | PDA bump seed |

---

## Instructions

### `initialize_config(hospital: Pubkey)`

Sets up the `ConsentConfig` singleton. Callable once by any account (the `init` constraint prevents re-execution). Establishes the hospital address and an empty doctor whitelist.

### `manage_doctor(doctor: Pubkey, add: bool)`

Adds or removes a doctor from the whitelist.

- **Authority:** hospital only
- **Limit:** maximum 5 whitelisted doctors at any time
- Pass `add: true` to whitelist, `add: false` to remove

### `create_patient(name, age, gender, patient_hash, surrogate)`

Registers a patient and creates a `PatientAccount`.

- **Authority:** hospital only
- If `age < 18`, `surrogate` must be provided — an adult wallet that will sign consents on the patient's behalf

### `create_consent(consent_id: u64, procedure_template_id: u64, document_hash)`

Creates a new `ConsentAccount` with initial status `Pending`. Requires the patient to already have a registered `PatientAccount`. The `hospital` field is populated from the `ConsentConfig`, not from the caller.

- **Authority:** hospital or any whitelisted doctor

### `sign_consent()`

Patient or surrogate signs the consent. Status remains `Pending` — this starts the **24-hour approval window**.

- **Authority:** patient's own wallet (age ≥ 18) or the registered surrogate (age < 18)
- Emits `PatientSigned` with `signed_by` for audit

### `approve_consent()`

Whitelisted doctor approves the consent within the 24-hour window. Immediately transitions status to `Signed`.

- **Authority:** whitelisted doctor only
- **Restriction:** requires patient to have signed first; blocked if the window has already closed (call `expire_consent` instead)
- Emits `ConsentSigned` with `approved_by` for audit

### `expire_consent()`

Formally marks the consent as `Expired`. Permissionless — anyone can call this once the 24-hour window has passed without both parties approving. Solana has no timers; expiry must be triggered explicitly.

- **Restriction:** requires patient to have signed; requires window to have passed

### `update_document_hash(document_hash)`

Updates the SHA-256 hash of the consent document to reflect a change in terms.

- **Authority:** whitelisted doctor only
- **Restriction:** blocked once the patient has signed — the document is immutable from that point

### `withdraw_consent()`

Withdraws a consent. Hospital-only.

- **Valid transitions:** `Pending → Withdrawn`, `Signed → Withdrawn`

### `emergency_override(reason_hash, document_hash)`

Forces a consent to `Overridden` status from any state. Records a `reason_hash` on-chain for auditability.

- **Authority:** hospital only

---

## Consent lifecycle

```
        ┌─────────┐
        │ Pending │  ← created by hospital / doctor
        └────┬────┘
             │  sign_consent()  (patient or surrogate)
             │  document hash locked from this point
             ▼
        ┌─────────┐  24-hour window opens
        │ Pending │  waiting for doctor approval
        └────┬────┘
             │
     ┌───────┴───────────────┐
     │  approve_consent()    │  window closes (24h) without doctor approval
     │  (whitelisted doctor) │
     ▼                       ▼
 ┌────────┐            ┌─────────┐
 │ Signed │            │ Expired │  (expire_consent, permissionless)
 └───┬────┘            └─────────┘
     │  withdraw_consent() (hospital only)
     ▼
 ┌───────────┐
 │ Withdrawn │◄── Pending (hospital can cancel before signing too)
 └───────────┘

  Any state ──► Overridden  (emergency_override, hospital only)
```

---

## Events

All state changes emit on-chain events, providing an auditable log without requiring account storage for historical data.

| Event | Emitted by | Key fields |
|---|---|---|
| `PatientCreated` | `create_patient` | patient_hash, age, hospital |
| `ConsentCreated` | `create_consent` | consent_id, procedure_template_id, document_hash, status |
| `PatientSigned` | `sign_consent` | consent_id, signed_by, timestamp |
| `ConsentSigned` | `approve_consent` | consent_id, approved_by, timestamp, version |
| `ConsentExpired` | `expire_consent` | consent_id, timestamp, version |
| `DocumentHashUpdated` | `update_document_hash` | consent_id, document_hash, status, version |
| `StatusUpdated` | `withdraw_consent` | consent_id, document_hash, status, version |
| `EmergencyOverrideEvent` | `emergency_override` | consent_id, reason_hash, document_hash, version |

---

## Access control

| Instruction | Allowed callers | Notes |
|---|---|---|
| `initialize_config` | Anyone (once) | |
| `manage_doctor` | Hospital | |
| `create_patient` | Hospital | |
| `create_consent` | Hospital or whitelisted doctor | |
| `sign_consent` | Patient (age ≥ 18) or surrogate (age < 18) | Starts 24h window |
| `approve_consent` | Whitelisted doctor | Must approve within 24h of patient signing |
| `expire_consent` | Anyone | Only callable after 24h window |
| `update_document_hash` | Whitelisted doctor | Blocked after patient signs |
| `withdraw_consent` | Hospital | |
| `emergency_override` | Hospital | |

---

## Error codes

| Code | Description |
|---|---|
| `InvalidTransition` | Attempted status transition is not permitted by the state machine |
| `Unauthorized` | Signer does not match the required authority |
| `NotHospitalOrDoctor` | Caller is neither the hospital nor a whitelisted doctor |
| `NotWhitelistedDoctor` | Caller is not a whitelisted doctor |
| `DoctorAlreadyWhitelisted` | The provided doctor is already on the whitelist |
| `DoctorNotFound` | The provided doctor is not on the whitelist |
| `DoctorLimitReached` | Whitelist is at capacity (max 5 doctors) |
| `DocumentLocked` | Document hash cannot be modified after the patient has signed |
| `SurrogateRequired` | Patient is a minor but no surrogate address was provided |
| `ConsentWindowExpired` | The 24-hour approval window has closed; call `expire_consent` |
| `ApprovalWindowStillOpen` | `expire_consent` called before the 24-hour window has passed |

---

## Local development

### Prerequisites

- [Rust](https://rustup.rs/) (stable)
- [Solana CLI](https://docs.solana.com/cli/install-solana-cli-tools)
- [Anchor CLI](https://www.anchor-lang.com/docs/installation) 0.31.1
- [Node.js](https://nodejs.org/) + [pnpm](https://pnpm.io/)

### Setup

```bash
# Install JS dependencies
pnpm install

# Build the program
anchor build

# Run tests against a local validator
anchor test
```

---

## Security notes

- **No PHI on-chain.** The program only accepts hashed values. The caller is responsible for hashing patient identifiers and document contents before submitting transactions.
- **Authority is immutable.** The `hospital` field on a `ConsentAccount` is set at creation from the config and never updated — even if the config changes later, existing consents retain their original authority.
- **Minor patient protection.** Patients under 18 cannot sign their own consents. A surrogate address must be registered at patient creation time and is the only wallet permitted to sign on their behalf.
- **Document integrity after signing.** Once a consent reaches `Signed` status, the `document_hash` field is frozen. Neither doctors nor the hospital can alter it — only `emergency_override` (which transitions to `Overridden`) can proceed past that point.
- **Emergency override is traceable.** The `reason_hash` parameter ensures overrides carry an off-chain justification that can be verified against the on-chain record.
