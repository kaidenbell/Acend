# AcendCredit

**Closer to mid.**  
Most of your swap settles in lending markets — pools only close the gap.

## Paths

| Cluster | What works |
|---|---|
| **Devnet** | Quote + Tier-1 seed + **deposit send** (money-in). Flash/borrow blocked on-chain. |
| **Mainnet** | Mint-aligned compose: flash + borrow USDC + deposit SOL + **Orca residual splice** (simulate). |

## Quick start (Devnet money-in)

```bash
cd C:\Users\kaide\AcendCredit
cd scripts && npm install && node devnet-setup.js && cd ..
# fund .keys pubkey via https://faucet.solana.com if needed
cargo run -p acend-cli -- swap --pair SOL/USDC --amount-usd 25 --keypair .keys/devnet.json --send
```

## Mainnet compose (simulate — no auto-send)

```bash
$env:ACEND_PAIRS_CONFIG="config/pairs.mainnet.toml"
$env:ACEND_RPC_URL="https://api.mainnet-beta.solana.com"
$env:ACEND_MFI_ENV="production"
cargo run -p acend-cli -- swap --pair SOL/USDC --amount-usd 100
```

## Tier-1 bids

```bash
cargo run -p acend-cli -- seed-bid --pair SOL/USDC --max-spread-bps 0.75 --amount-usd 100000
node scripts/bidder-bot.js --pair SOL/USDC --max-spread-bps 0.75
```

## API (for your frontend)

```bash
cargo run -p acend-api
# Contract: docs/API.md
# GET /quote?pair=SOL/USDC&amount_usd=100
# GET /swap?pair=SOL/USDC&amount_usd=100&payer=<WALLET_PUBKEY>
# GET /bids  GET /pairs  GET /health  GET /metrics
```

Pass `payer` so the response is **partially signed** (`needs_client_signature: true`). Your app signs the fee-payer and sends.

## Venues

- marginfi mainnet program `MFv2…` group `4qp6…` SOL/USDC banks preloaded
- Orca SOL/USDC 0.04% `Czfq3xZZDmsdGdUyrNLtRhGc47cXcZtLG4crryfu44zE`
- ALTs: marginfi lookup tables compiled into v0 txs when available

## Still hardening

- Mainnet **funded send** (real USDC/SOL) — tiny size + CLI `--send` only when ready
- Dual-sign takeover co-signing (bid book loads from file; handoff not live yet)