use acend_core::{all_in_bps, split_notional, PairConfig};
use anyhow::{anyhow, Context, Result};
use orca_whirlpools::{
    set_native_mint_wrapping_strategy, swap_instructions, NativeMintWrappingStrategy,
    SwapInstructions, SwapType,
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
        })
    }

    /// Build real Whirlpool swap ixs for the residual notional (base → quote).
    pub async fn build_residual_swap(
        &self,
        rpc_url: &str,
        pair: &PairConfig,
        residual_usd: f64,
        base_price_usd: f64,
        payer: Pubkey,
        slippage_bps: u16,
    ) -> Result<OrcaSwapBuild> {
        if pair.whirlpool.trim().is_empty() {
            return Err(anyhow!("pair {} has no whirlpool configured", pair.id));
        }
        if residual_usd <= 0.0 || base_price_usd <= 0.0 {
            return Err(anyhow!("invalid residual/price"));
        }

        let whirlpool = Pubkey::from_str(&pair.whirlpool).context("whirlpool pubkey")?;
        let base_mint = Pubkey::from_str(&pair.base_mint).context("base mint")?;

        let base_tokens = residual_usd / base_price_usd;
        let scale = 10f64.powi(pair.base_decimals as i32);
        let input_amount_atoms = (base_tokens * scale).floor() as u64;
        if input_amount_atoms == 0 {
            return Err(anyhow!("residual too small for swap atoms"));
        }

        info!(
            pair = %pair.id,
            residual_usd,
            input_amount_atoms,
            %whirlpool,
            "building Orca residual swap"
        );

        // Prefer ATAs for WSOL so we don't need ephemeral keypair signers.
        let _ = set_native_mint_wrapping_strategy(NativeMintWrappingStrategy::Ata);

        // Tick-array quoting is stack-heavy on Windows default stacks.
        let rpc_url = rpc_url.to_string();
        let built: SwapInstructions = tokio::task::spawn_blocking(move || {
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
                        base_mint,
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
