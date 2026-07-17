use std::path::{Path, PathBuf};
use std::sync::Arc;

use acend_book::{new_bid, BidBook};
use acend_composer::Composer;
use acend_core::{load_pairs_config, QuoteRequest};
use acend_quote::QuoteEngine;
use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use solana_sdk::signature::{Keypair, Signer};
use tracing::Level;

#[derive(Parser, Debug)]
#[command(name = "acend-cli", about = "AcendCredit CLI")]
struct Args {
    #[arg(long, env = "ACEND_PAIRS_CONFIG", default_value = "config/pairs.toml")]
    pairs: PathBuf,

    #[arg(
        long,
        env = "ACEND_RPC_URL",
        default_value = "https://api.devnet.solana.com"
    )]
    rpc: String,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    Quote {
        #[arg(long)]
        pair: String,
        #[arg(long)]
        amount_usd: f64,
    },
    SeedBid {
        #[arg(long)]
        pair: String,
        #[arg(long, default_value_t = 150_000.0)]
        max_size_usd: f64,
        #[arg(long, default_value_t = 0.75)]
        max_spread_bps: f64,
        #[arg(long, default_value_t = 100_000.0)]
        amount_usd: f64,
        #[arg(long)]
        ltv_bps: Option<u32>,
    },
    /// Compose marginfi flash+LTV (Orca deferred). Optionally send with funded keypair.
    Swap {
        #[arg(long)]
        pair: String,
        #[arg(long)]
        amount_usd: f64,
        #[arg(long, env = "ACEND_KEYPAIR")]
        keypair: Option<PathBuf>,
        #[arg(long, default_value_t = false)]
        send: bool,
    },
    /// Compose separate Orca residual tx (Devnet 2-tx path).
    SwapResidual {
        #[arg(long)]
        pair: String,
        #[arg(long)]
        amount_usd: f64,
        #[arg(long, env = "ACEND_KEYPAIR")]
        keypair: Option<PathBuf>,
        #[arg(long, default_value_t = false)]
        send: bool,
    },
    Health,
}

fn expand_path(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) {
            return PathBuf::from(home).join(rest);
        }
    }
    p.to_path_buf()
}

fn load_keypair(path: &Path) -> Result<Keypair> {
    let path = expand_path(path);
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("read keypair {}", path.display()))?;
    let bytes: Vec<u8> = serde_json::from_str(&raw).context("parse keypair json")?;
    Keypair::try_from(bytes.as_slice()).map_err(|e| anyhow!("keypair bytes: {e}"))
}

fn resolve_payer(keypair: Option<PathBuf>, send: bool) -> Result<Keypair> {
    match keypair {
        Some(path) => load_keypair(&path),
        None => {
            if send {
                anyhow::bail!("--send requires --keypair / ACEND_KEYPAIR");
            }
            Ok(Keypair::new())
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(Level::INFO)
        .init();

    let args = Args::parse();
    let config = load_pairs_config(args.pairs.to_str().unwrap())?;
    let book = BidBook::new();
    let engine = QuoteEngine::new(config.clone(), Arc::clone(&book), args.rpc.clone());
    let composer = Composer::new(args.rpc);

    match args.cmd {
        Cmd::Health => {
            let msg = composer.health().await?;
            println!("{msg}");
        }
        Cmd::Quote { pair, amount_usd } => {
            let q = engine
                .quote(QuoteRequest {
                    pair,
                    amount_usd,
                    sell_base: true,
                })
                .await?;
            println!("{}", serde_json::to_string_pretty(&q)?);
        }
        Cmd::SeedBid {
            pair,
            max_size_usd,
            max_spread_bps,
            amount_usd,
            ltv_bps,
        } => {
            let pair_cfg = config.get(&pair)?;
            let before = engine
                .quote(QuoteRequest {
                    pair: pair.clone(),
                    amount_usd,
                    sell_base: true,
                })
                .await?;

            let kp = Keypair::new();
            let preferred_ltv = ltv_bps.unwrap_or(pair_cfg.ltv_bps);
            let bid = new_bid(
                pair.clone(),
                max_size_usd,
                max_spread_bps,
                preferred_ltv,
                kp.pubkey().to_string(),
            );
            book.upsert(bid.clone()).await;
            let after = engine
                .quote(QuoteRequest {
                    pair,
                    amount_usd,
                    sell_base: true,
                })
                .await?;
            println!(
                "seeded bid {} @ {:.2} bps / LTV {}\n  before: tier={} bps={:.3} out=${:.2}\n  after:  tier={} bps={:.3} out=${:.2}",
                bid.id,
                bid.max_spread_bps,
                preferred_ltv,
                before.tier.as_str(),
                before.bps_vs_mid,
                before.amount_out_usd,
                after.tier.as_str(),
                after.bps_vs_mid,
                after.amount_out_usd,
            );
            println!("{}", serde_json::to_string_pretty(&after)?);
        }
        Cmd::Swap {
            pair,
            amount_usd,
            keypair,
            send,
        } => {
            let pair_cfg = config.get(&pair)?.clone();
            let q = engine
                .quote(QuoteRequest {
                    pair,
                    amount_usd,
                    sell_base: true,
                })
                .await?;
            let payer = resolve_payer(keypair, send)?;
            eprintln!("payer={}", payer.pubkey());
            let payload = composer.compose_lfrs(&q, &pair_cfg, &payer, send).await?;
            println!("{}", serde_json::to_string_pretty(&payload)?);
        }
        Cmd::SwapResidual {
            pair,
            amount_usd,
            keypair,
            send,
        } => {
            let pair_cfg = config.get(&pair)?.clone();
            let q = engine
                .quote(QuoteRequest {
                    pair,
                    amount_usd,
                    sell_base: true,
                })
                .await?;
            let payer = resolve_payer(keypair, send)?;
            eprintln!("payer={}", payer.pubkey());
            let payload = composer
                .compose_orca_residual(&q, &pair_cfg, &payer, send)
                .await?;
            println!("{}", serde_json::to_string_pretty(&payload)?);
        }
    }

    Ok(())
}