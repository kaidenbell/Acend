#!/usr/bin/env node
/**
 * Tier-1 standing bid bot (scaffolding).
 * Posts competitive max_spread_bps into a local JSON book the API/CLI can load later.
 *
 *   node scripts/bidder-bot.js --pair SOL/USDC --max-spread-bps 0.75 --max-size-usd 150000
 */
const fs = require("fs");
const path = require("path");
const { Keypair } = require("@solana/web3.js");
const crypto = require("crypto");

function arg(name, fallback) {
  const i = process.argv.indexOf(`--${name}`);
  if (i === -1) return fallback;
  return process.argv[i + 1];
}

const pair = arg("pair", "SOL/USDC");
const maxSpread = parseFloat(arg("max-spread-bps", "0.75"));
const maxSize = parseFloat(arg("max-size-usd", "150000"));
const ltv = parseInt(arg("ltv-bps", "8000"), 10);
const outPath = arg(
  "out",
  path.join(__dirname, "..", "config", "standing-bids.json")
);

const kp = Keypair.generate();
const bid = {
  id: crypto.randomUUID(),
  pair,
  max_size_usd: maxSize,
  max_spread_bps: maxSpread,
  preferred_ltv_bps: ltv,
  bidder_pubkey: kp.publicKey.toBase58(),
  updated_at: new Date().toISOString(),
  note: "Scaffold bid — dual-sign handoff not yet live; use CLI seed-bid for in-process quotes.",
};

let book = { bids: [] };
if (fs.existsSync(outPath)) {
  try {
    book = JSON.parse(fs.readFileSync(outPath, "utf8"));
  } catch (_) {}
}
book.bids = (book.bids || []).filter(
  (b) => !(b.pair === pair && b.bidder_pubkey === bid.bidder_pubkey)
);
book.bids.push(bid);
fs.mkdirSync(path.dirname(outPath), { recursive: true });
fs.writeFileSync(outPath, JSON.stringify(book, null, 2));
console.log("wrote", outPath);
console.log(JSON.stringify(bid, null, 2));
console.log(
  `\nQuote with Tier-1 in-process:\n  cargo run -p acend-cli -- seed-bid --pair ${pair} --max-spread-bps ${maxSpread} --amount-usd 100000`
);