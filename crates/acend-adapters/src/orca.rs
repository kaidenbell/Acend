use acend_core::{all_in_bps, split_notional, PairConfig};
use anyhow::{anyhow, Context, Result};
use orca_whirlpools::{
    set_native_mint_wrapping_strategy, swap_instructions, NativeMintWrappingStrategy, SwapQuote,
    SwapType,
};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{instruction::Instruction, pubkey::Pubkey, signature::Keypair};
use std::str::FromStr;
use tracing::info;

/// Orca Whirlpool residual quotes + live swap instruction builder.
#[derive(Debug, Clone, Default)]
pub struct OrcaAdapter;

#[derive(Debug, Clone)]
pub struct OrcaResidualQuote {
    pub residual_usd: f64,
    pub fee_usd: f64,
    pub impact_usd: f64,
    pub fee_bps: f64,
    pub all_in_bps_of_full: f64,
    /// "live" from Whirlpool quote, or "model" fallback.
    pub source: String,
}

#[derive(Debug)]
pub struct OrcaSwapBuild {
    pub instructions: Vec<Instruction>,
    pub additional_signers: Vec<Keypair>,
    pub input_amount_atoms: u64,
    pub whirlpool: Pubkey,
    pub estimated_out_note: String,
}

impl OrcaAdapter {
    pub fn quote_residual(
        &self,
        pair: &PairConfig,
        notional_usd: f64,
        residual_impact_bps: f64,
    ) -> Result<OrcaResidualQuote> {
        let (_lending, residual) = split_notional(notional_usd, pair.ltv_bps);
        let fee_usd = residual * (pair.orca_fee_bps / 10_000.0);
        let impact_usd = residual * (residual_impact_bps / 10_000.0);
        let all_in = all_in_bps(
            residual,
            notional_usd,
            pair.orca_fee_bps,
            residual_impact_bps,
            0.0,
        );
        Ok(OrcaResidualQuote {
            residual_usd: residual,
            fee_usd,
            impact_usd,
            fee_bps: pair.orca_fee_bps,
            all_in_bps_of_full: all_in,
            source: "model".into(),
        })
    }

    /// Live Whirlpool residual quote.
    /// `sell_base=true` → ExactIn base (SOL); `false` → ExactIn quote (USDC).
    pub async fn quote_residual_live(
        &self,
        rpc_url: &str,
        pair: &PairConfig,
        notional_usd: f64,
        base_price_usd: f64,
        quote_price_usd: f64,
        sell_base: bool,
    ) -> Result<OrcaResidualQuote> {
        let (_lending, residual) = split_notional(notional_usd, pair.ltv_bps);
        if pair.whirlpool.trim().is_empty() {
            return Err(anyhow!("no whirlpool for {}", pair.id));
        }
        if residual <= 0.0 || base_price_usd <= 0.0 || quote_price_usd <= 0.0 {
            return Err(anyhow!("invalid residual/prices"));
        }

        let whirlpool = Pubkey::from_str(&pair.whirlpool).context("whirlpool pubkey")?;
        let base_mint = Pubkey::from_str(&pair.base_mint).context("base mint")?;
        let quote_mint = Pubkey::from_str(&pair.quote_mint).context("quote mint")?;

        let (input_mint, scale_in, scale_out, in_price, out_price) = if sell_base {
            (
                base_mint,
                10f64.powi(pair.base_decimals as i32),
                10f64.powi(pair.quote_decimals as i32),
                base_price_usd,
                quote_price_usd,
            )
        } else {
            (
                quote_mint,
                10f64.powi(pair.quote_decimals as i32),
                10f64.powi(pair.base_decimals as i32),
                quote_price_usd,
                base_price_usd,
            )
        };

        let input_amount_atoms = ((residual / in_price) * scale_in).floor() as u64;
        if input_amount_atoms == 0 {
            return Err(anyhow!("residual too small"));
        }

        let _ = set_native_mint_wrapping_strategy(NativeMintWrappingStrategy::Ata);
        let rpc_url = rpc_url.to_string();
        let built = tokio::task::spawn_blocking(move || {
            std::thread::Builder::new()
                .name("orca-quote".into())
                .stack_size(32 * 1024 * 1024)
                .spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .expect("rt");
                    let rpc = RpcClient::new(rpc_url);
                    let payer = Pubkey::new_unique();
                    rt.block_on(swap_instructions(
                        &rpc,
                        whirlpool,
                        input_amount_atoms,
                        input_mint,
                        SwapType::ExactIn,
                        Some(50u16),
                        Some(payer),
                    ))
                    .map_err(|e| anyhow!("orca quote: {e}"))
                })
                .map_err(|e| anyhow!("spawn: {e}"))?
                .join()
                .map_err(|_| anyhow!("orca quote thread panicked"))?
        })
        .await
        .map_err(|e| anyhow!("join: {e}"))??;

        let SwapQuote::ExactIn(q) = built.quote else {
            return Err(anyhow!("expected ExactIn quote"));
        };

        let in_usd = (q.token_in as f64 / scale_in) * in_price;
        let out_usd = (q.token_est_out as f64 / scale_out) * out_price;
        let shortfall_usd = (in_usd - out_usd).max(0.0);
        let fee_usd = (q.trade_fee as f64 / scale_in) * in_price;
        let impact_usd = (shortfall_usd - fee_usd).max(0.0);
        let all_in = if notional_usd > 0.0 {
            (shortfall_usd / notional_usd) * 10_000.0
        } else {
            0.0
        };
        let fee_bps = if residual > 0.0 {
            (fee_usd / residual) * 10_000.0
        } else {
            pair.orca_fee_bps
        };

        info!(
            pair = %pair.id,
            sell_base,
            residual,
            in_usd,
            out_usd,
            shortfall_usd,
            all_in,
            "live Orca residual quote"
        );

        Ok(OrcaResidualQuote {
            residual_usd: residual,
            fee_usd,
            impact_usd,
            fee_bps,
            all_in_bps_of_full: all_in,
            source: "live".into(),
        })
    }

    /// Build Whirlpool ExactIn ixs for residual.
    /// `sell_base=true` → swap base→quote; `false` → quote→base.
    pub async fn build_residual_swap(
        &self,
        rpc_url: &str,
        pair: &PairConfig,
        residual_usd: f64,
        base_price_usd: f64,
        quote_price_usd: f64,
        payer: Pubkey,
        slippage_bps: u16,
        sell_base: bool,
    ) -> Result<OrcaSwapBuild> {
        if pair.whirlpool.trim().is_empty() {
            return Err(anyhow!("pair {} has no whirlpool configured", pair.id));
        }
        if residual_usd <= 0.0 || base_price_usd <= 0.0 || quote_price_usd <= 0.0 {
            return Err(anyhow!("invalid residual/price"));
        }

        let whirlpool = Pubkey::from_str(&pair.whirlpool).context("whirlpool pubkey")?;
        let base_mint = Pubkey::from_str(&pair.base_mint).context("base mint")?;
        let quote_mint = Pubkey::from_str(&pair.quote_mint).context("quote mint")?;

        let (input_mint, in_price, scale) = if sell_base {
            (
                base_mint,
                base_price_usd,
                10f64.powi(pair.base_decimals as i32),
            )
        } else {
            (
                quote_mint,
                quote_price_usd,
                10f64.powi(pair.quote_decimals as i32),
            )
        };

        let input_amount_atoms = ((residual_usd / in_price) * scale).floor() as u64;
        if input_amount_atoms == 0 {
            return Err(anyhow!("residual too small for swap atoms"));
        }

        info!(
            pair = %pair.id,
            sell_base,
            residual_usd,
            input_amount_atoms,
            %whirlpool,
            "building Orca residual swap"
        );

        let _ = set_native_mint_wrapping_strategy(NativeMintWrappingStrategy::Ata);

        let rpc_url = rpc_url.to_string();
        let built = tokio::task::spawn_blocking(move || {
            std::thread::Builder::new()
                .name("orca-swap".into())
                .stack_size(32 * 1024 * 1024)
                .spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .expect("rt");
                    let rpc = RpcClient::new(rpc_url);
                    rt.block_on(swap_instructions(
                        &rpc,
                        whirlpool,
                        input_amount_atoms,
                        input_mint,
                        SwapType::ExactIn,
                        Some(slippage_bps),
                        Some(payer),
                    ))
                    .map_err(|e| anyhow!("orca swap_instructions: {e}"))
                })
                .map_err(|e| anyhow!("spawn orca thread: {e}"))?
                .join()
                .map_err(|_| anyhow!("orca thread panicked"))?
        })
        .await
        .map_err(|e| anyhow!("orca join: {e}"))??;

        let estimated_out_note = format!("orca_quote={:?}", built.quote);

        Ok(OrcaSwapBuild {
            instructions: built.instructions,
            additional_signers: built.additional_signers,
            input_amount_atoms,
            whirlpool,
            estimated_out_note,
        })
    }
}
