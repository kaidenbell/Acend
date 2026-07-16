const { Connection, Keypair, PublicKey, SystemProgram } = require('@solana/web3.js');
const { getAssociatedTokenAddressSync, createAssociatedTokenAccountIdempotentInstruction, TOKEN_PROGRAM_ID, NATIVE_MINT } = require('@solana/spl-token');
const { MarginfiClient, getConfig } = require('@mrgnlabs/marginfi-client-v2');
const { NodeWallet } = require('@mrgnlabs/mrgn-common');
const { BN } = require('@coral-xyz/anchor');

(async () => {
  const connection = new Connection('https://api.devnet.solana.com', 'confirmed');
  const authority = Keypair.generate();
  const wallet = new NodeWallet(authority);
  const config = getConfig('dev');
  const client = await MarginfiClient.fetch(config, wallet, connection);
  const solBank = [...client.banks.values()].find(b => b.mint.equals(NATIVE_MINT));
  const quoteBank = [...client.banks.values()].find(b => b.mint.toBase58().startsWith('4Bn9'));
  const accountKp = Keypair.generate();
  const quoteAta = getAssociatedTokenAddressSync(quoteBank.mint, authority.publicKey, true);
  const solAta = getAssociatedTokenAddressSync(NATIVE_MINT, authority.publicKey, true);

  const [vaultAuth] = PublicKey.findProgramAddressSync(
    [Buffer.from('liquidity_vault_auth'), quoteBank.address.toBuffer()],
    config.programId
  );
  console.log('vaultAuth', vaultAuth.toBase58(), 'liqVault', quoteBank.liquidityVault.toBase58());

  // Use accountsPartial with only required, let anchor resolve
  try {
    const borrowIx = await client.program.methods
      .lendingAccountBorrow(new BN(1000000))
      .accounts({
        marginfiAccount: accountKp.publicKey,
        bank: quoteBank.address,
        destinationTokenAccount: quoteAta,
        tokenProgram: TOKEN_PROGRAM_ID,
      })
      .accountsPartial({
        group: config.groupPk,
        authority: authority.publicKey,
      })
      .remainingAccounts([
        { pubkey: quoteBank.oracleKey, isWritable: false, isSigner: false },
      ])
      .instruction();
    console.log('borrow ok', borrowIx.keys.length);
  } catch (e) {
    console.error('borrow fail', e.message);
  }

  // Deposit
  try {
    const depositIx = await client.program.methods
      .lendingAccountDeposit(new BN(100000000), null)
      .accounts({
        marginfiAccount: accountKp.publicKey,
        bank: solBank.address,
        signerTokenAccount: solAta,
        tokenProgram: TOKEN_PROGRAM_ID,
      })
      .accountsPartial({
        group: config.groupPk,
        authority: authority.publicKey,
      })
      .instruction();
    console.log('deposit ok', depositIx.keys.length, depositIx.keys.map(k => k.pubkey.toBase58().slice(0,6)));
  } catch (e) {
    console.error('deposit fail', e.message);
  }
})().catch(e => { console.error(e); process.exit(1); });
