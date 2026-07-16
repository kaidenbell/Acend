use std::collections::HashMap;
use std::sync::Arc;

use acend_core::SettlementTier;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StandingBid {
    pub id: String,
    pub pair: String,
    pub max_size_usd: f64,
    /// Max auction spread in bps the bidder accepts.
    pub max_spread_bps: f64,
    pub preferred_ltv_bps: u32,
    pub bidder_pubkey: String,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Default)]
pub struct BidBook {
    inner: RwLock<HashMap<String, Vec<StandingBid>>>,
}

impl BidBook {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub async fn upsert(&self, bid: StandingBid) {
        let mut g = self.inner.write().await;
        let entry = g.entry(bid.pair.clone()).or_default();
        if let Some(existing) = entry.iter_mut().find(|b| b.bidder_pubkey == bid.bidder_pubkey)
        {
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

/// Map book hit → settlement tier preference.
pub fn tier_from_book(has_bid: bool, has_opposite: bool) -> SettlementTier {
    if has_bid {
        SettlementTier::Takeover
    } else if has_opposite {
        SettlementTier::Net
    } else {
        SettlementTier::OrcaFallback
    }
}
