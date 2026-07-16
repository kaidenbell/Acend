//! Notional split + bps math for credit-settled quotes.

/// Split trade notional into lending-financed vs residual (pool) legs.
pub fn split_notional(notional_usd: f64, ltv_bps: u32) -> (f64, f64) {
    let ltv = (ltv_bps as f64 / 10_000.0).clamp(0.0, 0.99);
    let lending = notional_usd * ltv;
    let residual = notional_usd - lending;
    (lending, residual)
}

pub fn lending_share_usd(notional_usd: f64, ltv_bps: u32) -> f64 {
    split_notional(notional_usd, ltv_bps).0
}

pub fn residual_share_usd(notional_usd: f64, ltv_bps: u32) -> f64 {
    split_notional(notional_usd, ltv_bps).1
}

/// Pool fee on residual only, expressed as bps of *full* notional.
pub fn residual_fee_bps_of_full(residual_usd: f64, notional_usd: f64, pool_fee_bps: f64) -> f64 {
    if notional_usd <= 0.0 {
        return 0.0;
    }
    let fee_usd = residual_usd * (pool_fee_bps / 10_000.0);
    (fee_usd / notional_usd) * 10_000.0
}

/// All-in haircut in bps of full notional vs mid.
pub fn all_in_bps(
    residual_usd: f64,
    notional_usd: f64,
    pool_fee_bps: f64,
    residual_impact_bps: f64,
    auction_spread_bps: f64,
) -> f64 {
    let fee = residual_fee_bps_of_full(residual_usd, notional_usd, pool_fee_bps);
    // Impact also only hits residual, scale to full notional.
    let impact = if notional_usd <= 0.0 {
        0.0
    } else {
        (residual_usd / notional_usd) * residual_impact_bps
    };
    fee + impact + auction_spread_bps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_75_ltv() {
        let (l, r) = split_notional(150_000.0, 7500);
        assert!((l - 112_500.0).abs() < 1e-6);
        assert!((r - 37_500.0).abs() < 1e-6);
    }

    #[test]
    fn residual_fee_small_vs_full_pool() {
        // 0.04% on $37.5k residual ≈ 1 bp of $150k
        let bps = residual_fee_bps_of_full(37_500.0, 150_000.0, 4.0);
        assert!((bps - 1.0).abs() < 1e-9);
    }

    #[test]
    fn all_in_under_two_bps() {
        let bps = all_in_bps(37_500.0, 150_000.0, 4.0, 2.0, 0.5);
        // fee 1.0 + impact 0.5 + spread 0.5 = 2.0
        assert!((bps - 2.0).abs() < 1e-9);
    }
}
