# AcendCredit API contract

Base URL (local): `http://127.0.0.1:8080`

All JSON. CORS is open for local integration.

## `GET /health`

```json
{ "ok": true, "rpc": "solana … slot=…" }
```

## `GET /pairs`

Array of configured pair objects from `config/pairs.toml` (or `ACEND_PAIRS_CONFIG`).

## `GET /quote`

| Query | Type | Notes |
|---|---|---|
| `pair` | string | e.g. `SOL/USDC` |
| `amount_usd` | number | notional |
| `sell_base` | bool | default `true` |

Returns a `Quote` (tier, bps_vs_mid, breakdown, pyth prices, etc.). Standing bids from `config/standing-bids.json` feed Tier-1 when size fits.

## `GET /swap`

Compose a versioned transaction for your frontend to sign/send. **No server-side send.**

| Query | Type | Notes |
|---|---|---|
| `pair` | string | required |
| `amount_usd` | number | required |
| `path` | string | `lfrs` (default) or `residual` (Orca-only) |
| `payer` | string | **client wallet pubkey** — partially signed for this fee-payer |
| `strict` | bool | default `true` when `payer` set; `simulated_ok` only if sim is clean |

### Client integration (recommended)

```
GET /swap?pair=SOL/USDC&amount_usd=100&payer=<WALLET_PUBKEY>
```

Response fields:

| Field | Meaning |
|---|---|
| `quote` | Same shape as `/quote` |
| `transaction_base64` | bincode `VersionedTransaction` (base64) |
| `needs_client_signature` | `true` when fee-payer is unsigned — **you must sign** |
| `simulated_ok` | Under `strict=true`, only clean sim |
| `simulation_logs` | RPC sim logs |
| `stages` | Compose stages (marginfi / orca / fallback) |
| `payer` | Fee-payer pubkey used |
| `recent_blockhash` | Blockhash baked into the message |
| `lookup_tables` | ALT addresses used (if any) |
| `note` | Human-readable status |

Frontend flow:

1. `GET /quote` → show bps / tier  
2. `GET /swap?payer=<wallet>` → deserialize `transaction_base64`  
3. Wallet signs (fee-payer)  
4. `sendRawTransaction` / wallet send  

Without `payer`, the API composes against an ephemeral key (inspect / soft sim only). Prefer always passing `payer`.

## `GET /bids`

Standing bids currently loaded into the in-memory book (from `ACEND_BIDS_CONFIG`, default `config/standing-bids.json`).

## `GET /metrics`

Fill counters for this process (`fills_24h`, takeover/fallback %). Also written to `config/metrics.json`.

## Env

| Var | Default |
|---|---|
| `ACEND_BIND` | `127.0.0.1:8080` |
| `ACEND_RPC_URL` | Devnet RPC |
| `ACEND_PAIRS_CONFIG` | `config/pairs.toml` |
| `ACEND_BIDS_CONFIG` | `config/standing-bids.json` (local) / `config/standing-bids.deploy.json` (Railway) |
| `PORT` | Set by Railway — API binds `0.0.0.0:$PORT` when `ACEND_BIND` unset |
