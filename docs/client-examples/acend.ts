/**
 * Drop into your Next/Vercel app.
 *
 * SECURITY (important):
 * - Put the Railway URL + API key in *server* env only when possible:
 *     ACEND_API_URL=https://….up.railway.app
 *     ACEND_API_KEY=…          (NOT NEXT_PUBLIC_)
 * - Call Railway from Next Route Handlers (`app/api/acend/...`) so the key
 *   never ships to the browser.
 * - Browser WebSockets cannot set headers; if you must connect WS from the
 *   client, pass ?key= (weaker — anyone who opens DevTools sees it). Prefer
 *   a server proxy or accept that risk for testing only.
 *
 * Client-visible (weaker) env for quick tests:
 *   NEXT_PUBLIC_ACEND_API_URL=…
 *   NEXT_PUBLIC_ACEND_API_KEY=…
 */

export type AcendQuote = {
  id: string
  pair: string
  tier: string
  amount_in_usd: number
  amount_out_usd: number
  mid_usd: number
  bps_vs_mid: number
  bps_cap: number
  pyth_base: number
  pyth_quote: number
  breakdown: {
    lending_usd: number
    residual_usd: number
    lending_pct: number
    residual_pct: number
    pool_fee_usd: number
    estimated_impact_usd: number
    auction_spread_usd: number
  }
  expires_at: string
  cluster: string
}

export type AcendSwapPayload = {
  quote: AcendQuote
  transaction_base64: string | null
  needs_client_signature: boolean
  simulated_ok: boolean
  note: string
  payer?: string
  recent_blockhash?: string
}

function apiBase(): string {
  const base = (
    process.env.ACEND_API_URL ||
    process.env.NEXT_PUBLIC_ACEND_API_URL ||
    ""
  ).replace(/\/$/, "")
  if (!base) throw new Error("Set ACEND_API_URL (server) or NEXT_PUBLIC_ACEND_API_URL")
  return base
}

function apiKey(): string | undefined {
  return process.env.ACEND_API_KEY || process.env.NEXT_PUBLIC_ACEND_API_KEY || undefined
}

function authHeaders(): HeadersInit {
  const key = apiKey()
  return key ? { "X-Acend-Key": key } : {}
}

function wsBase(): string {
  return apiBase().replace(/^http/, "ws")
}

export async function fetchAcendQuote(opts: {
  pair: string
  amountUsd: number
  sellBase?: boolean
}): Promise<AcendQuote> {
  const q = new URLSearchParams({
    pair: opts.pair,
    amount_usd: String(opts.amountUsd),
    sell_base: String(opts.sellBase ?? true),
  })
  const res = await fetch(`${apiBase()}/quote?${q}`, { headers: authHeaders() })
  if (!res.ok) throw new Error(await res.text())
  return res.json()
}

export async function fetchAcendSwap(opts: {
  pair: string
  amountUsd: number
  payer: string
  path?: "lfrs" | "residual"
  strict?: boolean
  sellBase?: boolean
}): Promise<AcendSwapPayload> {
  const q = new URLSearchParams({
    pair: opts.pair,
    amount_usd: String(opts.amountUsd),
    payer: opts.payer,
    path: opts.path ?? "lfrs",
    strict: String(opts.strict ?? true),
    sell_base: String(opts.sellBase ?? true),
  })
  const res = await fetch(`${apiBase()}/swap?${q}`, { headers: authHeaders() })
  if (!res.ok) throw new Error(await res.text())
  return res.json()
}

export type AcendTick = {
  quote: AcendQuote
  pyth_base: number
  pyth_quote: number
  ts_ms: number
}

export function subscribeAcendQuotes(opts: {
  pair: string
  amountUsd: number
  intervalMs?: number
  onTick: (tick: AcendTick) => void
  onError?: (err: string) => void
  onStatus?: (msg: string) => void
}): () => void {
  const intervalMs = Math.min(10_000, Math.max(1_000, opts.intervalMs ?? 2_000))
  const url = new URL(`${wsBase()}/ws/quotes`)
  url.searchParams.set("pair", opts.pair)
  url.searchParams.set("amount_usd", String(opts.amountUsd))
  url.searchParams.set("interval_ms", String(intervalMs))
  const key = apiKey()
  if (key) url.searchParams.set("key", key)

  let ws: WebSocket | null = null
  let closed = false
  let retryMs = 1_000

  const connect = () => {
    if (closed) return
    ws = new WebSocket(url.toString())

    ws.onopen = () => {
      retryMs = 1_000
      opts.onStatus?.("connected")
      ws?.send(
        JSON.stringify({
          op: "subscribe",
          pair: opts.pair,
          amount_usd: opts.amountUsd,
          interval_ms: intervalMs,
        }),
      )
    }

    ws.onmessage = (ev) => {
      try {
        const msg = JSON.parse(String(ev.data))
        if (msg.type === "tick") {
          opts.onTick({
            quote: msg.quote,
            pyth_base: msg.pyth_base,
            pyth_quote: msg.pyth_quote,
            ts_ms: msg.ts_ms,
          })
        } else if (msg.type === "error") {
          opts.onError?.(msg.error ?? "unknown")
        }
      } catch (e) {
        opts.onError?.(String(e))
      }
    }

    ws.onclose = () => {
      opts.onStatus?.("disconnected")
      if (closed) return
      const wait = retryMs
      retryMs = Math.min(15_000, retryMs * 2)
      setTimeout(connect, wait)
    }

    ws.onerror = () => opts.onError?.("websocket error")
  }

  connect()

  return () => {
    closed = true
    try {
      ws?.close()
    } catch {
      /* ignore */
    }
  }
}
