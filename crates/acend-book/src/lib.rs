use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use acend_core::SettlementTier;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StandingBid {
    pub id: String,
    pub pair: String,
    pub max_size_usd: f64,
    pub max_spread_bps: f64,
    pub preferred_ltv_bps: u32,
    pub bidder_pubkey: String,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
struct BidFile {
    #[serde(default)]
    bids: Vec<StandingBid>,
}

#[derive(Debug, Default)]
pub struct BidBook {
    inner: RwLock<HashMap<String, Vec<StandingBid>>>,
}

impl BidBook {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub async fn load_file(self: &Arc<Self>, path: impl AsRef<Path>) -> Result<usize> {
        let path = path.as_ref();
        if !path.exists() {
            return Ok(0);
        }
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("read bids {}", path.display()))?;
        let file: BidFile = serde_json::from_str(&raw).context("parse bids json")?;
        let n = file.bids.len();
        for bid in file.bids {
            self.upsert(bid).await;
        }
        Ok(n)
    }

    pub async fn upsert(&self, bid: StandingBid) {
        let mut g = self.inner.write().await;
        let entry = g.entry(bid.pair.clone()).or_default();
        if let Some(existing) = entry.iter_mut().find(|b| b.bidder_pubkey == bid.bidder_pubkey) {
            *existing = bid;
        } else {
            entry.push(bid);
        }
    }

    pub async fn best_for(&self, pair: &str, size_usd: f64) -> Option<StandingBid> {
        let g = self.inner.read().await;
        let mut bids: Vec<_> = g
            .get(pair)
            .into_iter()
            .flatten()
            .filter(|b| b.max_size_usd >= size_usd)
            .cloned()
            .collect();
        bids.sort_by(|a, b| {
            a.max_spread_bps
                .partial_cmp(&b.max_spread_bps)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        bids.into_iter().next()
    }

    pub async fn list(&self) -> Vec<StandingBid> {
        let g = self.inner.read().await;
        g.values().flat_map(|v| v.iter().cloned()).collect()
    }

    pub async fn remove(&self, pair: &str, bidder_pubkey: &str) {
        let mut g = self.inner.write().await;
        if let Some(entry) = g.get_mut(pair) {
            entry.retain(|b| b.bidder_pubkey != bidder_pubkey);
        }
    }
}

pub fn new_bid(
    pair: impl Into<String>,
    max_size_usd: f64,
    max_spread_bps: f64,
    preferred_ltv_bps: u32,
    bidder_pubkey: impl Into<String>,
) -> StandingBid {
    StandingBid {
        id: Uuid::new_v4().to_string(),
        pair: pair.into(),
        max_size_usd,
        max_spread_bps,
        preferred_ltv_bps,
        bidder_pubkey: bidder_pubkey.into(),
        updated_at: Utc::now(),
    }
}

pub fn tier_from_book(has_bid: bool, has_opposite: bool) -> SettlementTier {
    if has_bid {
        SettlementTier::Takeover
    } else if has_opposite {
        SettlementTier::Net
    } else {
        SettlementTier::OrcaFallback
    }
}

