use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::OnceLock;

const HERMES: &str = "https://hermes.pyth.network/v2/updates/price/latest";

fn feed_map() -> &'static HashMap<&'static str, &'static str> {
    static MAP: OnceLock<HashMap<&str, &str>> = OnceLock::new();
    MAP.get_or_init(|| {
        HashMap::from([
            (
                "Crypto.SOL/USD",
                "ef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d",
            ),
            (
                "Crypto.USDC/USD",
                "eaa020c61cc479712813461ce153894a96a6c00b21ed0cfc2798d1f9a9e9c94a",
            ),
            (
                "Crypto.USDT/USD",
                "2b89b9dc8fdf9f34709a5b106b472f0f39bb6ca9ce04b0fd7f2e971688e2e53b",
            ),
        ])
    })
}

#[derive(Debug, Clone)]
pub struct PythClient {
    http: reqwest::Client,
}

#[derive(Debug, Clone)]
pub struct PricePair {
    pub base_usd: f64,
    pub quote_usd: f64,
}

impl Default for PythClient {
    fn default() -> Self {
        Self::new()
    }
}

impl PythClient {
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("reqwest"),
        }
    }

    pub async fn prices(&self, base_id: &str, quote_id: &str) -> Result<PricePair> {
        let base = self.fetch_one(base_id).await?;
        let quote = self.fetch_one(quote_id).await?;
        Ok(PricePair {
            base_usd: base,
            quote_usd: quote,
        })
    }

    fn resolve_id(id: &str) -> Result<&'static str> {
        feed_map()
            .get(id)
            .copied()
            .or_else(|| {
                if id.len() == 64 && id.chars().all(|c| c.is_ascii_hexdigit()) {
                    // Can't return non-static; handle in fetch
                    None
                } else {
                    None
                }
            })
            .ok_or_else(|| anyhow!("unknown pyth feed id: {id}"))
    }

    async fn fetch_one(&self, id: &str) -> Result<f64> {
        let hex = if let Ok(h) = Self::resolve_id(id) {
            h.to_string()
        } else if id.len() == 64 && id.chars().all(|c| c.is_ascii_hexdigit()) {
            id.to_string()
        } else {
            return Self::fallback_price(id);
        };

        let url = format!("{HERMES}?ids[]={hex}");
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .with_context(|| format!("pyth fetch {id}"))?;

        if !resp.status().is_success() {
            return Self::fallback_price(id);
        }

        let body: HermesResponse = resp.json().await.context("pyth json")?;
        let parsed = match body.parsed.first() {
            Some(p) => p,
            None => return Self::fallback_price(id),
        };
        let price = parsed.price.price.parse::<f64>().context("price parse")?;
        let px = price * 10f64.powi(parsed.price.expo);
        if px <= 0.0 {
            return Self::fallback_price(id);
        }
        Ok(px)
    }

    fn fallback_price(id: &str) -> Result<f64> {
        match id {
            "Crypto.USDC/USD" | "Crypto.USDT/USD" => Ok(1.0),
            _ => Err(anyhow!("pyth unavailable for {id}")),
        }
    }
}

#[derive(Debug, Deserialize)]
struct HermesResponse {
    parsed: Vec<ParsedPrice>,
}

#[derive(Debug, Deserialize)]
struct ParsedPrice {
    price: PriceData,
}

#[derive(Debug, Deserialize)]
struct PriceData {
    price: String,
    expo: i32,
}
