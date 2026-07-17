# Railway setup (step-by-step)

You do **not** configure ports manually. Railway sets `PORT`; the API binds to it.

## 1. Networking (domain)

1. Open your Acend service → **Settings** → **Networking**
2. **Generate domain** if you don’t have one → copy  
   `https://….up.railway.app`
3. Custom domain is optional

Leave **TCP proxy / private networking** alone unless you know you need it.

## 2. Variables (this is the important part)

Service → **Variables** → add:

| Variable | Value | Required |
|---|---|---|
| `ACEND_API_KEY` | long random secret (password manager) | **Yes** |
| `ACEND_CORS_ORIGINS` | `https://your-app.vercel.app` (comma-separated if multiple) | **Yes** for browser |
| `ACEND_RPC_URL` | Helius/QuickNode mainnet URL | Recommended |
| `ACEND_PUBLIC_UI` | `false` | Optional (auto-false when key set) |

Do **not** set `PORT` or `ACEND_BIND`.

After saving variables, Railway redeploys.

## 3. What becomes private

With `ACEND_API_KEY` set:

| Path | Public? |
|---|---|
| `/health` | Yes (Railway healthcheck) |
| `/` demo UI | **No** (404) |
| `/quote` `/swap` `/ws/quotes` `/pairs` … | **Need key** |

REST header:

```http
X-Acend-Key: YOUR_KEY
```

WebSocket (browsers can’t set headers):

```
wss://….up.railway.app/ws/quotes?key=YOUR_KEY&pair=SOL/USDC&amount_usd=5
```

## 4. Vercel

**Best (key stays server-side):**

```
ACEND_API_URL=https://….up.railway.app
ACEND_API_KEY=same-as-railway
```

Call Railway only from Next Route Handlers / Server Actions.

**Quick test only (key visible in browser):**

```
NEXT_PUBLIC_ACEND_API_URL=https://….up.railway.app
NEXT_PUBLIC_ACEND_API_KEY=same-as-railway
```

## 5. Sanity checks

```bash
# should 401 without key
curl https://YOUR.up.railway.app/quote?pair=SOL/USDC&amount_usd=5

# should 200 with key
curl -H "X-Acend-Key: YOUR_KEY" \
  "https://YOUR.up.railway.app/quote?pair=SOL/USDC&amount_usd=5"

# health still open
curl https://YOUR.up.railway.app/health
```

## Honest limits

- A public URL can still be *found*; the key stops casual spam.
- A key in the **browser** (or `?key=` on WS) can be copied from DevTools — fine for early testing, not for high-value production.
- For production: keep the key on the **server** (Next API routes) and only allow your Vercel origin via `ACEND_CORS_ORIGINS`.
