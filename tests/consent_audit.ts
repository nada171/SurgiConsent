import * as anchor from "@coral-xyz/anchor";
import { Keypair, PublicKey } from "@solana/web3.js";
import { expect } from "chai";
import crypto from "crypto";

describe("surgi_consent", () => {
  anchor.setProvider(anchor.AnchorProvider.env());
  const provider = anchor.getProvider() as anchor.AnchorProvider;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const program = anchor.workspace.SurgiConsent as any;

  // ── PDA helpers ─────────────────────────────────────────────────────────────

  const [configPda] = PublicKey.findProgramAddressSync(
    [Buffer.from("consent_config")],
    program.programId
  );

  function patientPda(wallet: PublicKey): [PublicKey, number] {
    return PublicKey.findProgramAddressSync(
      [Buffer.from("patient"), wallet.toBuffer()],
      program.programId
    );
  }

  function consentPda(wallet: PublicKey, consentId: number): [PublicKey, number] {
    const idBytes = Buffer.alloc(8);
    idBytes.writeBigUInt64LE(BigInt(consentId));
    return PublicKey.findProgramAddressSync(
      [Buffer.from("consent"), wallet.toBuffer(), idBytes],
      program.programId
    );
  }

  // ── Shared wallets ───────────────────────────────────────────────────────────

  const hospital = Keypair.generate();
  const doctor1  = Keypair.generate();
  const doctor2  = Keypair.generate();

  async function airdrop(pubkey: PublicKey, lamports = 2_000_000_000) {
    const sig = await provider.connection.requestAirdrop(pubkey, lamports);
    await provider.connection.confirmTransaction(sig);
  }

  before(async () => {
    await Promise.all([
      airdrop(hospital.publicKey),
      airdrop(doctor1.publicKey),
      airdrop(doctor2.publicKey),
    ]);
  });

  // ── initialize_config ────────────────────────────────────────────────────────

  it("initializes the config (anyone, once)", async () => {
    await program.methods
      .initializeConfig(hospital.publicKey)
      .accounts({
        config: configPda,
        payer: provider.wallet.publicKey,
        systemProgram: anchor.web3.SystemProgram.programId,
      })
      .rpc();

    const config = await program.account.consentConfig.fetch(configPda);
    expect(config.hospital.toBase58()).to.eq(hospital.publicKey.toBase58());
    expect(config.whitelistedDoctors).to.have.length(0);
  });

  // ── manage_doctor ────────────────────────────────────────────────────────────

  it("hospital adds doctors to the whitelist", async () => {
    for (const doc of [doctor1, doctor2]) {
      await program.methods
        .manageDoctor(doc.publicKey, true)
        .accounts({ config: configPda, hospital: hospital.publicKey })
        .signers([hospital])
        .rpc();
    }

    const config = await program.account.consentConfig.fetch(configPda);
    expect(config.whitelistedDoctors).to.have.length(2);
  });

  it("rejects a duplicate doctor add", async () => {
    try {
      await program.methods
        .manageDoctor(doctor1.publicKey, true)
        .accounts({ config: configPda, hospital: hospital.publicKey })
        .signers([hospital])
        .rpc();
      expect.fail("should have thrown");
    } catch (e: any) {
      expect(String(e)).to.match(/DoctorAlreadyWhitelisted/i);
    }
  });

  it("hospital removes a doctor from the whitelist", async () => {
    await program.methods
      .manageDoctor(doctor2.publicKey, false)
      .accounts({ config: configPda, hospital: hospital.publicKey })
      .signers([hospital])
      .rpc();

    const config = await program.account.consentConfig.fetch(configPda);
    const keys = (config.whitelistedDoctors as PublicKey[]).map(k => k.toBase58());
    expect(keys).not.to.include(doctor2.publicKey.toBase58());
  });

  it("rejects manage_doctor from a non-hospital signer", async () => {
    const rando = Keypair.generate();
    await airdrop(rando.publicKey);

    try {
      await program.methods
        .manageDoctor(rando.publicKey, true)
        .accounts({ config: configPda, hospital: rando.publicKey })
        .signers([rando])
        .rpc();
      expect.fail("should have thrown");
    } catch (e: any) {
      expect(String(e)).to.match(/Unauthorized|ConstraintHasOne/i);
    }
  });

  it("emits DoctorManaged event on add and remove", async () => {
    const tempDoctor = Keypair.generate();

    // ── add ──────────────────────────────────────────────────────────────────
    const addEventPromise = new Promise<any>(resolve => {
      const id = program.addEventListener("DoctorManaged", (e: any) => {
        program.removeEventListener(id);
        resolve(e);
      });
    });

    await program.methods
      .manageDoctor(tempDoctor.publicKey, true)
      .accounts({ config: configPda, hospital: hospital.publicKey })
      .signers([hospital])
      .rpc();

    const addEvent = await addEventPromise;
    expect(addEvent.doctor.toBase58()).to.eq(tempDoctor.publicKey.toBase58());
    expect(addEvent.added).to.be.true;
    expect(addEvent.hospital.toBase58()).to.eq(hospital.publicKey.toBase58());
    expect(addEvent.timestamp.toNumber()).to.be.greaterThan(0);

    // ── remove ───────────────────────────────────────────────────────────────
    const removeEventPromise = new Promise<any>(resolve => {
      const id = program.addEventListener("DoctorManaged", (e: any) => {
        program.removeEventListener(id);
        resolve(e);
      });
    });

    await program.methods
      .manageDoctor(tempDoctor.publicKey, false)
      .accounts({ config: configPda, hospital: hospital.publicKey })
      .signers([hospital])
      .rpc();

    const removeEvent = await removeEventPromise;
    expect(removeEvent.doctor.toBase58()).to.eq(tempDoctor.publicKey.toBase58());
    expect(removeEvent.added).to.be.false;
    expect(removeEvent.hospital.toBase58()).to.eq(hospital.publicKey.toBase58());
    expect(removeEvent.timestamp.toNumber()).to.be.greaterThan(0);
  });

  // ── create_patient ───────────────────────────────────────────────────────────

  let patientWallet: Keypair;
  let patientAccountPda: PublicKey;

  it("hospital registers an adult patient", async () => {
    patientWallet = Keypair.generate();
    await airdrop(patientWallet.publicKey);
    [patientAccountPda] = patientPda(patientWallet.publicKey);

    await program.methods
      .createPatient(
        [...crypto.randomBytes(32)],
        false,
        null
      )
      .accounts({
        patientAccount: patientAccountPda,
        patientWallet: patientWallet.publicKey,
        config: configPda,
        hospital: hospital.publicKey,
        systemProgram: anchor.web3.SystemProgram.programId,
      })
      .signers([hospital])
      .rpc();

    const acct = await program.account.patientAccount.fetch(patientAccountPda);
    expect(acct.isMinor).to.be.false;
    expect(acct.surrogate).to.be.null;
  });

  it("rejects a minor patient with no surrogate", async () => {
    const minorWallet = Keypair.generate();
    const [minorPda] = patientPda(minorWallet.publicKey);

    try {
      await program.methods
        .createPatient([...crypto.randomBytes(32)], true, null)
        .accounts({
          patientAccount: minorPda,
          patientWallet: minorWallet.publicKey,
          config: configPda,
          hospital: hospital.publicKey,
          systemProgram: anchor.web3.SystemProgram.programId,
        })
        .signers([hospital])
        .rpc();
      expect.fail("should have thrown");
    } catch (e: any) {
      expect(String(e)).to.match(/SurrogateRequired/i);
    }
  });

  it("accepts a minor patient with a surrogate", async () => {
    const minorWallet  = Keypair.generate();
    const surrogateKey = Keypair.generate();
    const [minorPda]   = patientPda(minorWallet.publicKey);

    await program.methods
      .createPatient(
        [...crypto.randomBytes(32)],
        true,
        surrogateKey.publicKey
      )
      .accounts({
        patientAccount: minorPda,
        patientWallet: minorWallet.publicKey,
        config: configPda,
        hospital: hospital.publicKey,
        systemProgram: anchor.web3.SystemProgram.programId,
      })
      .signers([hospital])
      .rpc();

    const acct = await program.account.patientAccount.fetch(minorPda);
    expect(acct.isMinor).to.be.true;
    expect(acct.surrogate.toBase58()).to.eq(surrogateKey.publicKey.toBase58());
  });

  // ── create_consent ───────────────────────────────────────────────────────────

  const CONSENT_ID = 1;
  let consentPdaAddr: PublicKey;
  let currentDocHash: number[];

  it("whitelisted doctor creates a consent for the registered patient", async () => {
    [consentPdaAddr] = consentPda(patientWallet.publicKey, CONSENT_ID);
    currentDocHash = [...crypto.randomBytes(32)];

    await program.methods
      .createConsent(
        new anchor.BN(CONSENT_ID),
        new anchor.BN(42),
        currentDocHash
      )
      .accounts({
        consent: consentPdaAddr,
        patientWallet: patientWallet.publicKey,
        patientAccount: patientAccountPda,
        config: configPda,
        signer: doctor1.publicKey,
        systemProgram: anchor.web3.SystemProgram.programId,
      })
      .signers([doctor1])
      .rpc();

    const acct = await program.account.consentAccount.fetch(consentPdaAddr);
    expect(acct.status).to.deep.eq({ pending: {} });
    expect(acct.version.toNumber()).to.eq(1);
  });

  it("rejects consent creation by a non-hospital, non-doctor signer", async () => {
    const rando = Keypair.generate();
    await airdrop(rando.publicKey);
    const [randoConsentPda] = consentPda(patientWallet.publicKey, 999);

    try {
      await program.methods
        .createConsent(new anchor.BN(999), new anchor.BN(1), [...crypto.randomBytes(32)])
        .accounts({
          consent: randoConsentPda,
          patientWallet: patientWallet.publicKey,
          patientAccount: patientAccountPda,
          config: configPda,
          signer: rando.publicKey,
          systemProgram: anchor.web3.SystemProgram.programId,
        })
        .signers([rando])
        .rpc();
      expect.fail("should have thrown");
    } catch (e: any) {
      expect(String(e)).to.match(/NotHospitalOrDoctor/i);
    }
  });

  // ── update_document_hash ─────────────────────────────────────────────────────

  it("doctor updates the document hash before the patient signs", async () => {
    currentDocHash = [...crypto.randomBytes(32)];

    await program.methods
      .updateDocumentHash(currentDocHash)
      .accounts({
        consent: consentPdaAddr,
        patientWallet: patientWallet.publicKey,
        config: configPda,
        doctor: doctor1.publicKey,
      })
      .signers([doctor1])
      .rpc();

    const acct = await program.account.consentAccount.fetch(consentPdaAddr);
    expect(Buffer.from(acct.documentHash)).to.deep.eq(Buffer.from(currentDocHash));
    expect(acct.version.toNumber()).to.eq(2);
  });

  // ── sign_consent ─────────────────────────────────────────────────────────────

  it("patient signs the consent — status transitions to Signed, window opens", async () => {
    await program.methods
      .signConsent()
      .accounts({
        consent: consentPdaAddr,
        patientAccount: patientAccountPda,
        patientWallet: patientWallet.publicKey,
        signer: patientWallet.publicKey,
      })
      .signers([patientWallet])
      .rpc();

    const acct = await program.account.consentAccount.fetch(consentPdaAddr);
    expect(acct.status).to.deep.eq({ signed: {} });
    expect(acct.signedAt.toNumber()).to.be.greaterThan(0);
    expect(acct.approvalExpiresAt.toNumber()).to.be.greaterThan(acct.signedAt.toNumber());
  });

  it("blocks a second patient signature (InvalidTransition — already Signed)", async () => {
    try {
      await program.methods
        .signConsent()
        .accounts({
          consent: consentPdaAddr,
          patientAccount: patientAccountPda,
          patientWallet: patientWallet.publicKey,
          signer: patientWallet.publicKey,
        })
        .signers([patientWallet])
        .rpc();
      expect.fail("should have thrown");
    } catch (e: any) {
      expect(String(e)).to.match(/InvalidTransition/i);
    }
  });

  it("blocks document hash update after the patient has signed (DocumentLocked)", async () => {
    try {
      await program.methods
        .updateDocumentHash([...crypto.randomBytes(32)])
        .accounts({
          consent: consentPdaAddr,
          patientWallet: patientWallet.publicKey,
          config: configPda,
          doctor: doctor1.publicKey,
        })
        .signers([doctor1])
        .rpc();
      expect.fail("should have thrown");
    } catch (e: any) {
      expect(String(e)).to.match(/DocumentLocked/i);
    }
  });

  it("expire_consent is blocked while the 24-hour window is still open", async () => {
    try {
      await program.methods
        .expireConsent()
        .accounts({
          consent: consentPdaAddr,
          patientWallet: patientWallet.publicKey,
        })
        .rpc();
      expect.fail("should have thrown");
    } catch (e: any) {
      expect(String(e)).to.match(/ApprovalWindowStillOpen/i);
    }
  });

  // ── approve_consent ──────────────────────────────────────────────────────────

  it("approve_consent fails without a prior patient signature", async () => {
    // Create a fresh consent that has NOT been signed yet.
    const CONSENT_ID_UNSIGNED = 100;
    const [unsignedConsentPda] = consentPda(patientWallet.publicKey, CONSENT_ID_UNSIGNED);

    await program.methods
      .createConsent(new anchor.BN(CONSENT_ID_UNSIGNED), new anchor.BN(1), [...crypto.randomBytes(32)])
      .accounts({
        consent: unsignedConsentPda,
        patientWallet: patientWallet.publicKey,
        patientAccount: patientAccountPda,
        config: configPda,
        signer: doctor1.publicKey,
        systemProgram: anchor.web3.SystemProgram.programId,
      })
      .signers([doctor1])
      .rpc();

    try {
      await program.methods
        .approveConsent()
        .accounts({
          consent: unsignedConsentPda,
          patientWallet: patientWallet.publicKey,
          config: configPda,
          approver: doctor1.publicKey,
        })
        .signers([doctor1])
        .rpc();
      expect.fail("should have thrown");
    } catch (e: any) {
      expect(String(e)).to.match(/InvalidTransition/i);
    }
  });

  it("whitelisted doctor approves within the window → status becomes Approved", async () => {
    await program.methods
      .approveConsent()
      .accounts({
        consent: consentPdaAddr,
        patientWallet: patientWallet.publicKey,
        config: configPda,
        approver: doctor1.publicKey,
      })
      .signers([doctor1])
      .rpc();

    const acct = await program.account.consentAccount.fetch(consentPdaAddr);
    expect(acct.status).to.deep.eq({ approved: {} });
    expect(acct.version.toNumber()).to.eq(4);
  });

  it("rejects a non-whitelisted approver (NotWhitelistedDoctor)", async () => {
    const CONSENT_ID_2 = 2;
    const [consentPda2] = consentPda(patientWallet.publicKey, CONSENT_ID_2);

    await program.methods
      .createConsent(new anchor.BN(CONSENT_ID_2), new anchor.BN(7), [...crypto.randomBytes(32)])
      .accounts({
        consent: consentPda2,
        patientWallet: patientWallet.publicKey,
        patientAccount: patientAccountPda,
        config: configPda,
        signer: hospital.publicKey,
        systemProgram: anchor.web3.SystemProgram.programId,
      })
      .signers([hospital])
      .rpc();

    await program.methods
      .signConsent()
      .accounts({
        consent: consentPda2,
        patientAccount: patientAccountPda,
        patientWallet: patientWallet.publicKey,
        signer: patientWallet.publicKey,
      })
      .signers([patientWallet])
      .rpc();

    try {
      // doctor2 was removed from the whitelist earlier
      await program.methods
        .approveConsent()
        .accounts({
          consent: consentPda2,
          patientWallet: patientWallet.publicKey,
          config: configPda,
          approver: doctor2.publicKey,
        })
        .signers([doctor2])
        .rpc();
      expect.fail("should have thrown");
    } catch (e: any) {
      expect(String(e)).to.match(/NotWhitelistedDoctor/i);
    }
  });

  // ── withdraw_consent ─────────────────────────────────────────────────────────

  it("hospital withdraws an Approved consent → status becomes Withdrawn", async () => {
    await program.methods
      .withdrawConsent()
      .accounts({
        consent: consentPdaAddr,
        patientWallet: patientWallet.publicKey,
        hospital: hospital.publicKey,
      })
      .signers([hospital])
      .rpc();

    const acct = await program.account.consentAccount.fetch(consentPdaAddr);
    expect(acct.status).to.deep.eq({ withdrawn: {} });
  });

  it("rejects invalid transition Withdrawn → Signed", async () => {
    try {
      await program.methods
        .approveConsent()
        .accounts({
          consent: consentPdaAddr,
          patientWallet: patientWallet.publicKey,
          config: configPda,
          approver: doctor1.publicKey,
        })
        .signers([doctor1])
        .rpc();
      expect.fail("should have thrown");
    } catch (e: any) {
      expect(String(e)).to.match(/InvalidTransition/i);
    }
  });

  // ── emergency_override ───────────────────────────────────────────────────────

  it("hospital emergency-overrides a consent from any state", async () => {
    const CONSENT_ID_3 = 3;
    const [consentPda3] = consentPda(patientWallet.publicKey, CONSENT_ID_3);

    await program.methods
      .createConsent(new anchor.BN(CONSENT_ID_3), new anchor.BN(99), [...crypto.randomBytes(32)])
      .accounts({
        consent: consentPda3,
        patientWallet: patientWallet.publicKey,
        patientAccount: patientAccountPda,
        config: configPda,
        signer: hospital.publicKey,
        systemProgram: anchor.web3.SystemProgram.programId,
      })
      .signers([hospital])
      .rpc();

    await program.methods
      .emergencyOverride([...crypto.randomBytes(32)], [...crypto.randomBytes(32)])
      .accounts({
        consent: consentPda3,
        patientWallet: patientWallet.publicKey,
        hospital: hospital.publicKey,
      })
      .signers([hospital])
      .rpc();

    const acct = await program.account.consentAccount.fetch(consentPda3);
    expect(acct.status).to.deep.eq({ overridden: {} });
  });
});
