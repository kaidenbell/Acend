/**
 * Drop into your Vercel/Next app (e.g. lib/acend.ts).
 *
 * Env:
 *   NEXT_PUBLIC_ACEND_API_URL=https://YOUR-SERVICE.up.railway.app
 *
 * REST (compose once, when user clicks swap):
 *   const swap = await fetchAcendSwap({ pair, amountUsd, payer })
 *
 * Live quotes (WebSocket — do NOT poll /quote from Vercel):
 *   const stop = subscribeAcendQuotes({ pair, amountUsd, onTick })
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
  const base = process.env.NEXT_PUBLIC_ACEND_API_URL?.replace(/\/$/, "")
  if (!base) throw new Error("Set NEXT_PUBLIC_ACEND_API_URL to your Railway URL")
  return base
}

function wsBase(): string {
  const http = apiBase()
  return http.replace(/^http/, "ws")
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
  const res = await fetch(`${apiBase()}/quote?${q}`)
  if (!res.ok) throw new Error(await res.text())
  return res.json()
}

/** Call only when the user confirms a swap — builds a partially signed tx. */
export async function fetchAcendSwap(opts: {
  pair: string
  amountUsd: number
  payer: string
  path?: "lfrs" | "residual"
  strict?: boolean
}): Promise<AcendSwapPayload> {
  const q = new URLSearchParams({
    pair: opts.pair,
    amount_usd: String(opts.amountUsd),
    payer: opts.payer,
    path: opts.path ?? "lfrs",
    strict: String(opts.strict ?? true),
  })
  const res = await fetch(`${apiBase()}/swap?${q}`)
  if (!res.ok) throw new Error(await res.text())
  return res.json()
}

export type AcendTick = {
  quote: AcendQuote
  pyth_base: number
  pyth_quote: number
  ts_ms: number
}

/**
 * Live quote + Pyth prices over WebSocket (default every 2s).
 * Returns an unsubscribe function.
 */
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
        } else if (msg.type === "subscribed") {
          opts.onStatus?.(`subscribed ${msg.pair} @ $${msg.amount_usd}`)
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

    ws.onerror = () => {
      opts.onError?.("websocket error")
    }
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

/** Update notional on an open socket without reconnecting. */
export function acendWsSetAmount(ws: WebSocket, amountUsd: number) {
  ws.send(JSON.stringify({ op: "set", amount_usd: amountUsd }))
}
