# AcendCredit

**Closer to mid.**  
Most of your swap settles in lending markets — pools only close the gap.

Credit-settled exchange on Solana: ~80% via lending LTV, residual via Orca. Instant fills.

## Why AcendCredit (vs a pool router)

| | Pool routers | AcendCredit |
|---|---|---|
| Slippage at size | On full notional | Mostly on the ~20% gap |
| MEV surface | Full swap leg | Residual only |
| Mid discipline | Best route | Mid or no fill |
| Depth | LP inventory | Lending borrow capacity |

## Stack

- **Rust workspace** — quote engine, adapters, tx composer, API, CLI
- **No custom on-chain program** (v0) — off-chain composer builds atomic txs
- **Devnet first**

## Quick start

```bash
cd C:\Users\kaide\AcendCredit

# Quote SOL → USDC ($10k)
cargo run -p acend-cli -- quote --pair SOL/USDC --amount-usd 10000

# Ping Devnet
cargo run -p acend-cli -- health

# Compose + simulate Devnet demo tx
cargo run -p acend-cli -- swap --pair SOL/USDC --amount-usd 1000

# API + UI (http://127.0.0.1:8080)
cargo run -p acend-api
```

## Verified on Devnet

- Live Pyth mid (Hermes)
- Quote: 75% lending / 25% residual, ~1.375 bps vs mid on SOL/USDC
- RPC health against `https://api.devnet.solana.com`
- HTTP `/quote` + thin UI with locked hero + 4 cards
- **Live Orca Whirlpool residual CPI** on pool `3KBZiL…`
- **Live marginfi Devnet flash + LTV ixs**: init → start_flash → borrow → deposit → repay → end_flash
- LFRS stages published per fill; Orca residual deferred on Devnet when mint ≠ marginfi quote mint

## Next

- Funded wallet e2e send (not ephemeral)
- Single-tx merge once quote mints align (mainnet)
- Tier-1 dual-sign takeover bots
## Crates

| Crate | Role |
|-------|------|
| `acend-core` | Types, pair config, bps math |
| `acend-adapters` | Pyth, Orca fee model, lending LTV model |
| `acend-quote` | Tier ladder: takeover → net → Orca |
| `acend-composer` | VersionedTransaction + Devnet simulate |
| `acend-book` | Standing bid book |
| `acend-api` | `/quote` `/metrics` + UI |
| `acend-cli` | Terminal quote / swap / health |
