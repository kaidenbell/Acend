use serde::{Deserialize, Serialize};

use crate::error::{AcendError, Result};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PairsConfig {
    pub cluster: String,
    pub pairs: Vec<PairConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PairConfig {
    pub id: String,
    pub base: String,
    pub quote: String,
    pub max_size_usd: f64,
    pub bps_cap: f64,
    /// Loan-to-value in basis points (7500 = 75%).
    pub ltv_bps: u32,
    pub base_mint: String,
    pub quote_mint: String,
    pub pyth_base: String,
    pub pyth_quote: String,
    /// Expected Orca pool fee in bps (4.0 = 0.04%).
    pub orca_fee_bps: f64,
    /// Orca Whirlpool address (empty = no live residual pool yet).
    #[serde(default)]
    pub whirlpool: String,
    #[serde(default = "nine")]
    pub base_decimals: u8,
    #[serde(default = "six")]
    pub quote_decimals: u8,
}

fn nine() -> u8 {
    9
}
fn six() -> u8 {
    6
}

impl PairsConfig {
    pub fn get(&self, id: &str) -> Result<&PairConfig> {
        self.pairs
            .iter()
            .find(|p| p.id.eq_ignore_ascii_case(id))
            .ok_or_else(|| AcendError::PairNotFound(id.to_string()))
    }
}

pub fn load_pairs_config(path: &str) -> Result<PairsConfig> {
    let raw = std::fs::read_to_string(path)?;
    let cfg: PairsConfig =
        toml::from_str(&raw).map_err(|e| AcendError::Config(e.to_string()))?;
    if cfg.pairs.is_empty() {
        return Err(AcendError::Config("no pairs in config".into()));
    }
    Ok(cfg)
}
