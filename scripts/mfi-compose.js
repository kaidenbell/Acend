#!/usr/bin/env node
/**
 * Build live marginfi Devnet instructions for AcendCredit LFRS.
 *
 *   node scripts/mfi-compose.js \
 *     --authority <pubkey> \
 *     --sol-amount-atoms <u64> \
 *     --borrow-amount-atoms <u64>
 */
const { Connection, Keypair, PublicKey, SystemProgram } = require("@solana/web3.js");
const {
  getAssociatedTokenAddressSync,
  createAssociatedTokenAccountIdempotentInstruction,
  TOKEN_PROGRAM_ID,
  NATIVE_MINT,
} = require("@solana/spl-token");
const { MarginfiClient, getConfig } = require("@mrgnlabs/marginfi-client-v2");
const { NodeWallet } = require("@mrgnlabs/mrgn-common");
const { BN } = require("@coral-xyz/anchor");

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

(async () => {
  const rpc = arg("rpc", "https://api.devnet.solana.com");
  const authorityStr = arg("authority");
  if (!authorityStr) throw new Error("--authority required");
  const authority = new PublicKey(authorityStr);
  const solAtoms = BigInt(arg("sol-amount-atoms", "0"));
  const borrowAtoms = BigInt(arg("borrow-amount-atoms", "0"));

  const walletKp = Keypair.generate();
  const wallet = new NodeWallet(walletKp);
  const connection = new Connection(rpc, "confirmed");
  const config = getConfig("dev");
  const client = await MarginfiClient.fetch(config, wallet, connection);

  const solBank = [...client.banks.values()].find((b) => b.mint.equals(NATIVE_MINT));
  const quoteBank = [...client.banks.values()].find((b) =>
    b.mint.toBase58().startsWith("4Bn9")
  );
  if (!solBank || !quoteBank) throw new Error("Devnet SOL/quote banks not found");

  const accountKp = Keypair.generate();
  const quoteAta = getAssociatedTokenAddressSync(quoteBank.mint, authority, true);
  const solAta = getAssociatedTokenAddressSync(NATIVE_MINT, authority, true);

  const prelude = [];

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
        quoteAta,
        authority,
        quoteBank.mint
      ),
      "create_quote_ata"
    )
  );
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

  const body = [];

  if (borrowAtoms > 0n) {
    const borrowIx = await client.program.methods
      .lendingAccountBorrow(new BN(borrowAtoms.toString()))
      .accounts({
        marginfiAccount: accountKp.publicKey,
        bank: quoteBank.address,
        destinationTokenAccount: quoteAta,
        tokenProgram: TOKEN_PROGRAM_ID,
      })
      .accountsPartial({
        group: config.groupPk,
        authority,
      })
      .remainingAccounts([
        { pubkey: quoteBank.oracleKey, isSigner: false, isWritable: false },
        { pubkey: solBank.oracleKey, isSigner: false, isWritable: false },
      ])
      .instruction();
    body.push(serIx(borrowIx, "lending_account_borrow"));
  }

  if (solAtoms > 0n) {
    const depositIx = await client.program.methods
      .lendingAccountDeposit(new BN(solAtoms.toString()), null)
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
  }

  if (borrowAtoms > 0n) {
    const repayIx = await client.program.methods
      .lendingAccountRepay(new BN(borrowAtoms.toString()), null)
      .accounts({
        marginfiAccount: accountKp.publicKey,
        bank: quoteBank.address,
        signerTokenAccount: quoteAta,
        tokenProgram: TOKEN_PROGRAM_ID,
      })
      .accountsPartial({
        group: config.groupPk,
        authority,
      })
      .instruction();
    body.push(serIx(repayIx, "lending_account_repay"));
  }

  const startIx = {
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
      Buffer.from([14, 131, 33, 220, 81, 186, 180, 107]), // lendingAccountStartFlashloan
      Buffer.alloc(8), // endIndex patched by Rust
    ]),
  };

  const endIx = {
    programId: config.programId,
    keys: [
      { pubkey: accountKp.publicKey, isSigner: false, isWritable: true },
      { pubkey: authority, isSigner: true, isWritable: false },
      { pubkey: solBank.address, isSigner: false, isWritable: false },
      { pubkey: quoteBank.address, isSigner: false, isWritable: false },
      { pubkey: solBank.oracleKey, isSigner: false, isWritable: false },
      { pubkey: quoteBank.oracleKey, isSigner: false, isWritable: false },
    ],
    data: Buffer.from([105, 124, 201, 106, 153, 2, 8, 156]), // lendingAccountEndFlashloan
  };

  process.stdout.write(
    JSON.stringify({
      cluster: "devnet",
      program: config.programId.toBase58(),
      group: config.groupPk.toBase58(),
      account: accountKp.publicKey.toBase58(),
      accountSecret: Buffer.from(accountKp.secretKey).toString("base64"),
      authority: authority.toBase58(),
      solBank: solBank.address.toBase58(),
      quoteBank: quoteBank.address.toBase58(),
      quoteMint: quoteBank.mint.toBase58(),
      prelude,
      startFlash: serIx(startIx, "lending_account_start_flashloan"),
      body,
      endFlash: serIx(endIx, "lending_account_end_flashloan"),
      note: "Devnet marginfi live ixs. Demo repay uses unspent flash borrow (quote mint ≠ Orca USDC).",
    })
  );
})().catch((e) => {
  console.error(e);
  process.exit(1);
});
