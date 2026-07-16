use std::path::PathBuf;
use std::sync::Arc;

use acend_book::{new_bid, BidBook};
use acend_composer::Composer;
use acend_core::{load_pairs_config, QuoteRequest};
use acend_quote::QuoteEngine;
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
    /// Quote a credit-settled swap
    Quote {
        #[arg(long)]
        pair: String,
        #[arg(long)]
        amount_usd: f64,
    },
    /// Seed a standing bid (enables Tier-1 takeover quotes)
    SeedBid {
        #[arg(long)]
        pair: String,
        #[arg(long, default_value_t = 150_000.0)]
        max_size_usd: f64,
        #[arg(long, default_value_t = 1.5)]
        max_spread_bps: f64,
    },
    /// Build LFRS tx: planned flash/LTV + live Orca residual CPI, simulate on Devnet
    Swap {
        #[arg(long)]
        pair: String,
        #[arg(long)]
        amount_usd: f64,
    },
    /// Ping Devnet RPC
    Health,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(Level::INFO)
        .init();

    let args = Args::parse();
    let config = load_pairs_config(args.pairs.to_str().unwrap())?;
    let book = BidBook::new();
    let engine = QuoteEngine::new(config.clone(), Arc::clone(&book));
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
        } => {
            let kp = Keypair::new();
            let bid = new_bid(
                pair.clone(),
                max_size_usd,
                max_spread_bps,
                7500,
                kp.pubkey().to_string(),
            );
            book.upsert(bid.clone()).await;
            let q = engine
                .quote(QuoteRequest {
                    pair,
                    amount_usd: 10_000.0,
                    sell_base: true,
                })
                .await?;
            println!(
                "seeded bid {} → next quote tier={} bps={:.2}",
                bid.id,
                q.tier.as_str(),
                q.bps_vs_mid
            );
            println!("{}", serde_json::to_string_pretty(&q)?);
        }
        Cmd::Swap { pair, amount_usd } => {
            let pair_cfg = config.get(&pair)?.clone();
            let q = engine
                .quote(QuoteRequest {
                    pair,
                    amount_usd,
                    sell_base: true,
                })
                .await?;
            let payer = Keypair::new();
            let payload = composer.compose_lfrs(&q, &pair_cfg, &payer).await?;
            println!("{}", serde_json::to_string_pretty(&payload)?);
        }
    }

    Ok(())
}
