use std::sync::Arc;

use acend_adapters::{LendingAdapter, OrcaAdapter, PythClient};
use acend_book::{tier_from_book, BidBook};
use acend_core::{
    split_notional, AcendError, PairConfig, PairsConfig, Quote, QuoteBreakdown, QuoteRequest,
    Result, SettlementTier,
};
use chrono::{Duration, Utc};
use uuid::Uuid;

const MIN_LENDING_PCT: f64 = 60.0;
/// Assumed residual pool impact (bps of residual) when no live pool sim yet.
const DEFAULT_RESIDUAL_IMPACT_BPS: f64 = 1.5;

pub struct QuoteEngine {
    pub config: PairsConfig,
    pub pyth: PythClient,
    pub lending: LendingAdapter,
    pub orca: OrcaAdapter,
    pub book: Arc<BidBook>,
}

impl QuoteEngine {
    pub fn new(config: PairsConfig, book: Arc<BidBook>) -> Self {
        Self {
            config,
            pyth: PythClient::new(),
            lending: LendingAdapter,
            orca: OrcaAdapter,
            book,
        }
    }

    pub async fn quote(&self, req: QuoteRequest) -> Result<Quote> {
        let pair = self.config.get(&req.pair)?.clone();
        if req.amount_usd <= 0.0 {
            return Err(AcendError::Config("amount_usd must be > 0".into()));
        }
        if req.amount_usd > pair.max_size_usd {
            return Err(AcendError::OverMaxSize(req.amount_usd, pair.max_size_usd));
        }

        let prices = self
            .pyth
            .prices(&pair.pyth_base, &pair.pyth_quote)
            .await
            .map_err(|e| AcendError::Oracle(e.to_string()))?;

        // Mid value of the clip in USD (amount is already USD notional for MVP).
        let mid_usd = req.amount_usd;
        let _ = req.sell_base;
        let _ = prices.base_usd / prices.quote_usd;

        let bid = self.book.best_for(&pair.id, req.amount_usd).await;
        let tier = tier_from_book(bid.is_some(), false);

        let (inner, bps) = match tier {
            SettlementTier::Takeover => self.quote_takeover(&pair, mid_usd, &bid.unwrap())?,
            SettlementTier::Net => self.quote_orca_fallback(&pair, mid_usd)?, // net stub → orca
            SettlementTier::OrcaFallback => self.quote_orca_fallback(&pair, mid_usd)?,
        };

        if bps > pair.bps_cap {
            return Err(AcendError::OverBpsCap {
                got: bps,
                cap: pair.bps_cap,
            });
        }

        if inner.breakdown.lending_pct < MIN_LENDING_PCT {
            return Err(AcendError::LendingShareTooLow {
                got: inner.breakdown.lending_pct,
            });
        }

        Ok(Quote {
            id: Uuid::new_v4().to_string(),
            pair: pair.id.clone(),
            tier: inner.tier,
            amount_in_usd: mid_usd,
            amount_out_usd: mid_usd * (1.0 - bps / 10_000.0),
            mid_usd,
            bps_vs_mid: bps,
            bps_cap: pair.bps_cap,
            pyth_base: prices.base_usd,
            pyth_quote: prices.quote_usd,
            breakdown: inner.breakdown,
            expires_at: Utc::now() + Duration::seconds(30),
            cluster: self.config.cluster.clone(),
        })
    }

    fn quote_takeover(
        &self,
        pair: &PairConfig,
        mid_usd: f64,
        bid: &acend_book::StandingBid,
    ) -> Result<(InnerQuote, f64)> {
        let (lending_usd, residual_usd) = split_notional(mid_usd, bid.preferred_ltv_bps);
        // Takeover: residual is bidder equity — no pool fee; only auction spread.
        let spread_bps = bid.max_spread_bps.min(pair.bps_cap);
        let spread_usd = mid_usd * (spread_bps / 10_000.0);
        let breakdown = QuoteBreakdown {
            lending_usd,
            residual_usd,
            lending_pct: (lending_usd / mid_usd) * 100.0,
            residual_pct: (residual_usd / mid_usd) * 100.0,
            pool_fee_usd: 0.0,
            estimated_impact_usd: 0.0,
            auction_spread_usd: spread_usd,
        };
        Ok((
            InnerQuote {
                tier: SettlementTier::Takeover,
                breakdown,
            },
            spread_bps,
        ))
    }

    fn quote_orca_fallback(&self, pair: &PairConfig, mid_usd: f64) -> Result<(InnerQuote, f64)> {
        let lending = self
            .lending
            .quote(pair, mid_usd)
            .map_err(|e| AcendError::Compose(e.to_string()))?;
        let orca = self
            .orca
            .quote_residual(pair, mid_usd, DEFAULT_RESIDUAL_IMPACT_BPS)
            .map_err(|e| AcendError::Compose(e.to_string()))?;

        let (lending_usd, residual_usd) = (lending.lending_usd, orca.residual_usd);
        let breakdown = QuoteBreakdown {
            lending_usd,
            residual_usd,
            lending_pct: (lending_usd / mid_usd) * 100.0,
            residual_pct: (residual_usd / mid_usd) * 100.0,
            pool_fee_usd: orca.fee_usd,
            estimated_impact_usd: orca.impact_usd,
            auction_spread_usd: 0.0,
        };
        Ok((
            InnerQuote {
                tier: SettlementTier::OrcaFallback,
                breakdown,
            },
            orca.all_in_bps_of_full,
        ))
    }
}

struct InnerQuote {
    tier: SettlementTier,
    breakdown: QuoteBreakdown,
}
