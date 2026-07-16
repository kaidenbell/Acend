use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use acend_book::BidBook;
use acend_composer::Composer;
use acend_core::{load_pairs_config, FillMetrics, QuoteRequest};
use acend_quote::QuoteEngine;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use axum::{Json, Router};
use clap::Parser;
use serde::Deserialize;
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
        env = "ACEND_RPC_URL",
        default_value = "https://api.devnet.solana.com"
    )]
    rpc: String,
}

#[derive(Clone)]
struct AppState {
    engine: Arc<QuoteEngine>,
    composer: Arc<Composer>,
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
    let engine = Arc::new(QuoteEngine::new(config, book));
    let composer = Arc::new(Composer::new(args.rpc));

    let state = AppState {
        engine,
        composer,
        fills: Arc::new(AtomicU64::new(0)),
        takeover: Arc::new(AtomicU64::new(0)),
        fallback: Arc::new(AtomicU64::new(0)),
    };

    let app = Router::new()
        .route("/", get(index))
        .route("/health", get(health))
        .route("/quote", get(quote))
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
                acend_core::SettlementTier::Takeover | acend_core::SettlementTier::Net => {
                    st.takeover.fetch_add(1, Ordering::Relaxed);
                }
                acend_core::SettlementTier::OrcaFallback => {
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
    Json(FillMetrics {
        takeover_pct,
        netted_pct: 0.0,
        fallback_pct,
        median_bps_vs_pyth: 0.0,
        median_lending_pct: 75.0,
        fills_24h: fills,
    })
}

async fn pairs(State(st): State<AppState>) -> impl IntoResponse {
    Json(st.engine.config.pairs.clone())
}
