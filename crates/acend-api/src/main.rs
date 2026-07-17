use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use acend_book::BidBook;
use acend_composer::{ComposeOpts, Composer};
use acend_core::{load_pairs_config, FillMetrics, QuoteRequest, SettlementTier};
use acend_quote::QuoteEngine;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use clap::Parser;
use serde::Deserialize;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Keypair;
use solana_sdk::signer::Signer;
use tower_http::cors::CorsLayer;
use tracing::{info, Level};

#[derive(Parser, Debug)]
#[command(name = "acend-api", about = "AcendCredit quote/swap API")]
struct Args {
    #[arg(long, env = "ACEND_BIND", default_value = "127.0.0.1:8080")]
    bind: SocketAddr,

    #[arg(long, env = "ACEND_PAIRS_CONFIG", default_value = "config/pairs.toml")]
    pairs: PathBuf,

    #[arg(
        long,
        env = "ACEND_BIDS_CONFIG",
        default_value = "config/standing-bids.json"
    )]
    bids: PathBuf,

    #[arg(
        long,
        env = "ACEND_RPC_URL",
        default_value = "https://api.devnet.solana.com"
    )]
    rpc: String,
}

#[derive(Clone)]
struct AppState {
    engine: Arc<QuoteEngine>,
    composer: Arc<Composer>,
    book: Arc<BidBook>,
    fills: Arc<AtomicU64>,
    takeover: Arc<AtomicU64>,
    fallback: Arc<AtomicU64>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(Level::INFO)
        .init();

    let args = Args::parse();
    let config = load_pairs_config(args.pairs.to_str().unwrap())?;
    let book = BidBook::new();
    match book.load_file(&args.bids).await {
        Ok(n) => info!("loaded {n} standing bids from {}", args.bids.display()),
        Err(e) => tracing::warn!("bids load skipped: {e}"),
    }
    let engine = Arc::new(QuoteEngine::new(config, book.clone(), args.rpc.clone()));
    let composer = Arc::new(Composer::new(args.rpc));

    let state = AppState {
        engine,
        composer,
        book,
        fills: Arc::new(AtomicU64::new(0)),
        takeover: Arc::new(AtomicU64::new(0)),
        fallback: Arc::new(AtomicU64::new(0)),
    };

    let app = Router::new()
        .route("/", get(index))
        .route("/health", get(health))
        .route("/quote", get(quote))
        .route("/swap", get(swap))
        .route("/bids", get(bids))
        .route("/metrics", get(metrics))
        .route("/pairs", get(pairs))
        .layer(CorsLayer::permissive())
        .with_state(state);

    info!("AcendCredit API on http://{}", args.bind);
    let listener = tokio::net::TcpListener::bind(args.bind).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn index() -> Html<&'static str> {
    Html(include_str!("../../../web/index.html"))
}

async fn health(State(st): State<AppState>) -> impl IntoResponse {
    match st.composer.health().await {
        Ok(msg) => (StatusCode::OK, Json(serde_json::json!({ "ok": true, "rpc": msg }))).into_response(),
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({ "ok": false, "error": e.to_string() })),
        )
            .into_response(),
    }
}

#[derive(Debug, Deserialize)]
struct QuoteQuery {
    pair: String,
    amount_usd: f64,
    #[serde(default = "default_true")]
    sell_base: bool,
}

fn default_true() -> bool {
    true
}

async fn quote(State(st): State<AppState>, Query(q): Query<QuoteQuery>) -> impl IntoResponse {
    let req = QuoteRequest {
        pair: q.pair,
        amount_usd: q.amount_usd,
        sell_base: q.sell_base,
    };
    match st.engine.quote(req).await {
        Ok(quote) => {
            st.fills.fetch_add(1, Ordering::Relaxed);
            match quote.tier {
                SettlementTier::Takeover | SettlementTier::Net => {
                    st.takeover.fetch_add(1, Ordering::Relaxed);
                }
                SettlementTier::OrcaFallback => {
                    st.fallback.fetch_add(1, Ordering::Relaxed);
                }
            }
            (StatusCode::OK, Json(quote)).into_response()
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

#[derive(Debug, Deserialize)]
struct SwapQuery {
    pair: String,
    amount_usd: f64,
    /// residual = Orca-only tx; default = marginfi LFRS
    #[serde(default)]
    path: Option<String>,
    /// Client wallet pubkey — compose partially signed for this fee-payer.
    #[serde(default)]
    payer: Option<String>,
    /// When true (default if payer set), simulated_ok only on clean sim.
    #[serde(default)]
    strict: Option<bool>,
}

async fn swap(State(st): State<AppState>, Query(q): Query<SwapQuery>) -> Response {
    let pair_cfg = match st.engine.config.get(&q.pair) {
        Ok(p) => p.clone(),
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response();
        }
    };
    let req = QuoteRequest {
        pair: q.pair.clone(),
        amount_usd: q.amount_usd,
        sell_base: true,
    };
    let quote = match st.engine.quote(req).await {
        Ok(q) => q,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response();
        }
    };

    let path = q.path.clone().unwrap_or_else(|| "lfrs".into());
    let payer_str = q.payer.clone();
    let strict_flag = q.strict;

    let result = match payer_str {
        Some(payer_str) => {
            let payer = match Pubkey::from_str(&payer_str) {
                Ok(p) => p,
                Err(e) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(serde_json::json!({ "error": format!("invalid payer: {e}") })),
                    )
                        .into_response();
                }
            };
            let opts = ComposeOpts {
                send: false,
                strict: strict_flag.unwrap_or(true),
            };
            if path == "residual" {
                st.composer
                    .compose_orca_residual_for_payer(&quote, &pair_cfg, payer, opts)
                    .await
            } else {
                st.composer
                    .compose_lfrs_for_payer(&quote, &pair_cfg, payer, opts)
                    .await
            }
        }
        None => {
            // Inspect-only: ephemeral key, soft sim unless strict=true.
            let ephemeral = Keypair::new();
            let opts = ComposeOpts {
                send: false,
                strict: strict_flag.unwrap_or(false),
            };
            if path == "residual" {
                st.composer
                    .compose_orca_residual(&quote, &pair_cfg, &ephemeral, false)
                    .await
            } else if opts.strict {
                st.composer
                    .compose_lfrs_for_payer(&quote, &pair_cfg, ephemeral.pubkey(), opts)
                    .await
            } else {
                st.composer
                    .compose_lfrs(&quote, &pair_cfg, &ephemeral, false)
                    .await
            }
        }
    };

    match result {
        Ok(payload) => (StatusCode::OK, Json(payload)).into_response(),
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

async fn bids(State(st): State<AppState>) -> impl IntoResponse {
    Json(st.book.list().await)
}

fn persist_metrics(path: &str, m: &FillMetrics) {
    if let Ok(s) = serde_json::to_string_pretty(m) {
        let _ = std::fs::write(path, s);
    }
}

async fn metrics(State(st): State<AppState>) -> Json<FillMetrics> {
    let fills = st.fills.load(Ordering::Relaxed);
    let takeover = st.takeover.load(Ordering::Relaxed);
    let fallback = st.fallback.load(Ordering::Relaxed);
    let (takeover_pct, fallback_pct) = if fills == 0 {
        (0.0, 0.0)
    } else {
        (
            (takeover as f64 / fills as f64) * 100.0,
            (fallback as f64 / fills as f64) * 100.0,
        )
    };
    let m = FillMetrics {
        takeover_pct,
        netted_pct: 0.0,
        fallback_pct,
        median_bps_vs_pyth: 0.0,
        median_lending_pct: 80.0,
        fills_24h: fills,
    };
    persist_metrics("config/metrics.json", &m);
    Json(m)
}

async fn pairs(State(st): State<AppState>) -> impl IntoResponse {
    Json(st.engine.config.pairs.clone())
}
