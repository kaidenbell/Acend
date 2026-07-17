# AcendCredit API contract

Base URL (local): `http://127.0.0.1:8080`

All JSON. CORS is open for local integration.

## Connect from your Vercel app

1. Set env on Vercel:

```
NEXT_PUBLIC_ACEND_API_URL=https://YOUR-SERVICE.up.railway.app
```

2. Copy `docs/client-examples/acend.ts` into your app (e.g. `lib/acend.ts`).

3. **Live quotes / prices** — WebSocket (do not poll `/quote`):

```ts
const stop = subscribeAcendQuotes({
  pair: "SOL/USDC",
  amountUsd: 5,
  intervalMs: 2000,
  onTick: ({ quote, pyth_base }) => {
    // update UI: quote.bps_vs_mid, quote.amount_out_usd, pyth_base (SOL/USD)
  },
})
// on unmount: stop()
```

4. **Swap once** (user clicks confirm) — REST:

```ts
const payload = await fetchAcendSwap({
  pair: "SOL/USDC",
  amountUsd: 5,
  payer: wallet.publicKey.toBase58(),
})
// deserialize payload.transaction_base64 → wallet.sign → sendRawTransaction
```

### `GET /ws/quotes` (WebSocket)

Upgrade URL: `wss://YOUR-SERVICE.up.railway.app/ws/quotes?pair=SOL/USDC&amount_usd=5&interval_ms=2000`

Client → server JSON:

| Message | Purpose |
|---|---|
| `{"op":"subscribe","pair":"SOL/USDC","amount_usd":5,"interval_ms":2000}` | start stream |
| `{"op":"set","amount_usd":10}` | change notional live |
| `{"op":"unsubscribe"}` | stop ticks |
| `{"op":"ping"}` | keepalive |

Server → client:

| `type` | Payload |
|---|---|
| `hello` | service info |
| `subscribed` | pair / amount / interval |
| `tick` | full `quote` + `pyth_base` / `pyth_quote` + `ts_ms` |
| `error` | `{ error }` |
| `pong` | reply to ping |

Interval is clamped to **1–10 seconds** (default **2s**). Ticks do not inflate `/metrics` fill counters.

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
