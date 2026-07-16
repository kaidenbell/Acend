use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettlementTier {
    /// Leverage takeover via standing bidder.
    Takeover,
    /// Net against opposite flow.
    Net,
    /// Orca residual / full fallback.
    OrcaFallback,
}

impl SettlementTier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Takeover => "takeover",
            Self::Net => "net",
            Self::OrcaFallback => "orca_fallback",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuoteRequest {
    pub pair: String,
    pub amount_usd: f64,
    /// If true, base→quote (e.g. SOL→USDC). Default true.
    #[serde(default = "default_true")]
    pub sell_base: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuoteBreakdown {
    pub lending_usd: f64,
    pub residual_usd: f64,
    pub lending_pct: f64,
    pub residual_pct: f64,
    pub pool_fee_usd: f64,
    pub estimated_impact_usd: f64,
    pub auction_spread_usd: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Quote {
    pub id: String,
    pub pair: String,
    pub tier: SettlementTier,
    pub amount_in_usd: f64,
    pub amount_out_usd: f64,
    pub mid_usd: f64,
    pub bps_vs_mid: f64,
    pub bps_cap: f64,
    pub pyth_base: f64,
    pub pyth_quote: f64,
    pub breakdown: QuoteBreakdown,
    pub expires_at: DateTime<Utc>,
    pub cluster: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FillMetrics {
    pub takeover_pct: f64,
    pub netted_pct: f64,
    pub fallback_pct: f64,
    pub median_bps_vs_pyth: f64,
    pub median_lending_pct: f64,
    pub fills_24h: u64,
}
