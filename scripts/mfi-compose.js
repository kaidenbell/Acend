#!/usr/bin/env node
/**
 * Build marginfi instructions for AcendCredit.
 *
 * Devnet  (--env dev):        deposit-only (no flash on-chain)
 * Mainnet (--env production):
 *   sell-base=true  (SOL→USDC): flash + borrow USDC + deposit SOL
 *   sell-base=false (USDC→SOL): flash + borrow SOL  + deposit USDC
 *
 *   node scripts/mfi-compose.js \
 *     --env production|dev \
 *     --authority <pubkey> \
 *     --collateral-amount-atoms <u64> \
 *     --borrow-amount-atoms <u64> \
 *     --sell-base true|false \
 *     --rpc <url>
 */
const {
  Connection,
  Keypair,
  PublicKey,
  SystemProgram,
} = require("@solana/web3.js");
const {
  getAssociatedTokenAddressSync,
  createAssociatedTokenAccountIdempotentInstruction,
  createSyncNativeInstruction,
  TOKEN_PROGRAM_ID,
  NATIVE_MINT,
} = require("@solana/spl-token");
const { MarginfiClient, getConfig } = require("@mrgnlabs/marginfi-client-v2");
const { NodeWallet } = require("@mrgnlabs/mrgn-common");
const { BN } = require("@coral-xyz/anchor");

const MAINNET_SOL_BANK = "CCKtUs6Cgwo4aaQUmBPmyoApH2gUDErxNZCAntD6LYGh";
const MAINNET_USDC_BANK = "2s37akK2eyBbp8DZgCm7RtsaEz8eJP3Nxd4urLHQv7yB";

function arg(name, fallback) {
  const i = process.argv.indexOf(`--${name}`);
  if (i === -1) return fallback;
  return process.argv[i + 1];
}

function serIx(ix, name) {
  return {
    name,
    programId: ix.programId.toBase58(),
    data: Buffer.from(ix.data).toString("base64"),
    keys: ix.keys.map((k) => ({
      pubkey: k.pubkey.toBase58(),
      isSigner: k.isSigner,
      isWritable: k.isWritable,
    })),
  };
}

function stubFlash(name) {
  return {
    name,
    programId: SystemProgram.programId.toBase58(),
    data: Buffer.from([0]).toString("base64"),
    keys: [],
  };
}

(async () => {
  const env = arg("env", "dev");
  const rpcDefault =
    env === "production"
      ? "https://api.mainnet-beta.solana.com"
      : "https://api.devnet.solana.com";
  const rpc = arg("rpc", rpcDefault);
  const authorityStr = arg("authority");
  if (!authorityStr) throw new Error("--authority required");
  const authority = new PublicKey(authorityStr);
  const sellBase = String(arg("sell-base", "true")).toLowerCase() !== "false";
  const collateralAtoms = BigInt(
    arg("collateral-amount-atoms", arg("sol-amount-atoms", "0"))
  );
  const borrowAtoms = BigInt(arg("borrow-amount-atoms", "0"));

  const connection = new Connection(rpc, "confirmed");
  const config = getConfig(env === "production" ? "production" : "dev");
  const clientOpts =
    env === "production"
      ? {
          preloadedBankAddresses: [
            new PublicKey(MAINNET_SOL_BANK),
            new PublicKey(MAINNET_USDC_BANK),
          ],
          readOnly: true,
        }
      : { readOnly: true };

  const client = await MarginfiClient.fetch(
    config,
    new NodeWallet(Keypair.generate()),
    connection,
    clientOpts
  );

  const solBank =
    [...client.banks.values()].find((b) => b.mint.equals(NATIVE_MINT)) ||
    client.banks.get(MAINNET_SOL_BANK);
  const quoteBank =
    env === "production"
      ? client.banks.get(MAINNET_USDC_BANK) ||
        [...client.banks.values()].find(
          (b) =>
            b.mint.toBase58() === "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"
        )
      : [...client.banks.values()].find((b) =>
          b.mint.toBase58().startsWith("4Bn9")
        );

  if (!solBank || !quoteBank) throw new Error("SOL/quote banks not found");

  const accountKp = Keypair.generate();
  const solAta = getAssociatedTokenAddressSync(NATIVE_MINT, authority, true);
  const quoteAta = getAssociatedTokenAddressSync(quoteBank.mint, authority, true);

  const collateralBank = sellBase ? solBank : quoteBank;
  const borrowBank = sellBase ? quoteBank : solBank;
  const collateralAta = sellBase ? solAta : quoteAta;
  const borrowAta = sellBase ? quoteAta : solAta;

  const prelude = [];
  const body = [];

  const initIx = await client.program.methods
    .marginfiAccountInitialize()
    .accounts({
      marginfiGroup: config.groupPk,
      marginfiAccount: accountKp.publicKey,
      authority,
      feePayer: authority,
      systemProgram: SystemProgram.programId,
    })
    .instruction();
  prelude.push(serIx(initIx, "marginfi_account_initialize"));
  prelude.push(
    serIx(
      createAssociatedTokenAccountIdempotentInstruction(
        authority,
        solAta,
        authority,
        NATIVE_MINT
      ),
      "create_sol_ata"
    )
  );
  prelude.push(
    serIx(
      createAssociatedTokenAccountIdempotentInstruction(
        authority,
        quoteAta,
        authority,
        quoteBank.mint
      ),
      "create_quote_ata"
    )
  );

  let depositSafe = collateralAtoms > 0n ? collateralAtoms : 0n;
  if (sellBase) {
    if (depositSafe === 0n) depositSafe = 100_000_000n;
    if (depositSafe > 2_000_000_000n) depositSafe = 2_000_000_000n;
    prelude.push(
      serIx(
        SystemProgram.transfer({
          fromPubkey: authority,
          toPubkey: solAta,
          lamports: Number(depositSafe),
        }),
        "wrap_transfer_sol"
      )
    );
    prelude.push(serIx(createSyncNativeInstruction(solAta), "sync_native"));
  } else if (depositSafe === 0n) {
    throw new Error(
      "USDC→SOL requires --collateral-amount-atoms > 0 (USDC atoms in wallet ATA)"
    );
  }

  let startFlash;
  let endFlash;
  let note;

  if (env === "production" && borrowAtoms > 0n && depositSafe > 0n) {
    const borrowIx = await client.program.methods
      .lendingAccountBorrow(new BN(borrowAtoms.toString()))
      .accounts({
        marginfiAccount: accountKp.publicKey,
        bank: borrowBank.address,
        destinationTokenAccount: borrowAta,
        tokenProgram: TOKEN_PROGRAM_ID,
      })
      .accountsPartial({
        group: config.groupPk,
        authority,
      })
      .remainingAccounts([
        { pubkey: borrowBank.oracleKey, isSigner: false, isWritable: false },
        { pubkey: collateralBank.oracleKey, isSigner: false, isWritable: false },
      ])
      .instruction();
    if (borrowIx.keys[5] && !borrowIx.keys[5].isSigner) {
      borrowIx.keys[5].isWritable = true;
    }
    body.push(
      serIx(
        borrowIx,
        sellBase ? "lending_account_borrow_usdc" : "lending_account_borrow_sol"
      )
    );

    const depositIx = await client.program.methods
      .lendingAccountDeposit(new BN(depositSafe.toString()), null)
      .accounts({
        marginfiAccount: accountKp.publicKey,
        bank: collateralBank.address,
        signerTokenAccount: collateralAta,
        tokenProgram: TOKEN_PROGRAM_ID,
      })
      .accountsPartial({
        group: config.groupPk,
        authority,
      })
      .instruction();
    body.push(
      serIx(
        depositIx,
        sellBase ? "lending_account_deposit_sol" : "lending_account_deposit_usdc"
      )
    );

    startFlash = serIx(
      {
        programId: config.programId,
        keys: [
          { pubkey: accountKp.publicKey, isSigner: false, isWritable: true },
          { pubkey: authority, isSigner: true, isWritable: false },
          {
            pubkey: new PublicKey("Sysvar1nstructions1111111111111111111111111"),
            isSigner: false,
            isWritable: false,
          },
        ],
        data: Buffer.concat([
          Buffer.from([14, 131, 33, 220, 81, 186, 180, 107]),
          Buffer.alloc(8),
        ]),
      },
      "lending_account_start_flashloan"
    );

    const endIx = await client.program.methods
      .lendingAccountEndFlashloan()
      .accounts({
        marginfiAccount: accountKp.publicKey,
        authority,
      })
      .remainingAccounts([
        { pubkey: collateralBank.address, isSigner: false, isWritable: false },
        { pubkey: borrowBank.address, isSigner: false, isWritable: false },
        { pubkey: collateralBank.oracleKey, isSigner: false, isWritable: false },
        { pubkey: borrowBank.oracleKey, isSigner: false, isWritable: false },
      ])
      .instruction();
    endFlash = serIx(endIx, "lending_account_end_flashloan");
    note = sellBase
      ? "Mainnet LFRS SOL→USDC: flash + borrow USDC + deposit SOL. Orca residual spliced before end_flash."
      : "Mainnet LFRS USDC→SOL: flash + borrow SOL + deposit USDC. Orca residual (USDC→SOL) spliced before end_flash.";
  } else {
    if (sellBase && depositSafe > 0n) {
      const depositIx = await client.program.methods
        .lendingAccountDeposit(new BN(depositSafe.toString()), null)
        .accounts({
          marginfiAccount: accountKp.publicKey,
          bank: solBank.address,
          signerTokenAccount: solAta,
          tokenProgram: TOKEN_PROGRAM_ID,
        })
        .accountsPartial({
          group: config.groupPk,
          authority,
        })
        .instruction();
      body.push(serIx(depositIx, "lending_account_deposit"));
      note =
        "Devnet deposit-only e2e (flashloan ixs not on-chain / borrow caps). Money-in = wrap+deposit SOL.";
    } else if (!sellBase) {
      note =
        "USDC→SOL LFRS requires mainnet production flash path; skipped deposit-only reverse on this env.";
    } else {
      note = "No collateral atoms; nothing deposited.";
    }
    startFlash = stubFlash("lending_account_start_flashloan");
    endFlash = stubFlash("lending_account_end_flashloan");
  }

  const lookupTables = (client.lookupTablesAddresses || []).map((p) =>
    p.toBase58()
  );

  process.stdout.write(
    JSON.stringify({
      cluster: env === "production" ? "mainnet-beta" : "devnet",
      env,
      sellBase,
      program: config.programId.toBase58(),
      group: config.groupPk.toBase58(),
      account: accountKp.publicKey.toBase58(),
      accountSecret: Buffer.from(accountKp.secretKey).toString("base64"),
      authority: authority.toBase58(),
      solBank: solBank.address.toBase58(),
      quoteBank: quoteBank.address.toBase58(),
      quoteMint: quoteBank.mint.toBase58(),
      lookupTables,
      prelude,
      startFlash,
      body,
      endFlash,
      note,
    })
  );
})().catch((e) => {
  console.error(e);
  process.exit(1);
});
