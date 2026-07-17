/**
 * Create/fund a Devnet payer for AcendCredit e2e.
 * Usage: node scripts/devnet-setup.js
 * If RPC airdrop is rate-limited, fund via https://faucet.solana.com
 */
const fs = require("fs");
const path = require("path");
const { Connection, Keypair, LAMPORTS_PER_SOL } = require("@solana/web3.js");

async function main() {
  const root = path.join(__dirname, "..");
  const keysDir = path.join(root, ".keys");
  const keyPath = path.join(keysDir, "devnet.json");
  fs.mkdirSync(keysDir, { recursive: true });

  let kp;
  if (fs.existsSync(keyPath)) {
    const secret = JSON.parse(fs.readFileSync(keyPath, "utf8"));
    kp = Keypair.fromSecretKey(Uint8Array.from(secret));
    console.log("loaded", keyPath);
  } else {
    kp = Keypair.generate();
    fs.writeFileSync(keyPath, JSON.stringify(Array.from(kp.secretKey)));
    console.log("wrote", keyPath);
  }

  const rpc = process.env.ACEND_RPC_URL || "https://api.devnet.solana.com";
  const conn = new Connection(rpc, "confirmed");
  console.log("pubkey", kp.publicKey.toBase58());

  let bal = await conn.getBalance(kp.publicKey);
  console.log("balance_sol", bal / LAMPORTS_PER_SOL);
  if (bal < 2 * LAMPORTS_PER_SOL) {
    console.log("requesting airdrop 1 SOL…");
    try {
      const sig = await conn.requestAirdrop(kp.publicKey, 1 * LAMPORTS_PER_SOL);
      await conn.confirmTransaction(sig, "confirmed");
      bal = await conn.getBalance(kp.publicKey);
      console.log("balance_sol", bal / LAMPORTS_PER_SOL, "airdrop", sig);
    } catch (e) {
      console.error("airdrop failed:", e.message || e);
      console.error("Fund manually: https://faucet.solana.com →", kp.publicKey.toBase58());
    }
  }

  console.log("\nexport ACEND_KEYPAIR=" + keyPath);
  console.log(
    "cargo run -p acend-cli -- swap --pair SOL/USDC --amount-usd 25 --keypair " +
      keyPath +
      " --send"
  );
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});