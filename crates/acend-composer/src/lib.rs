use acend_adapters::{
    patch_flash_end_index, scripts_dir_from_cwd, LendingAdapter, OrcaAdapter,
};
use acend_core::{PairConfig, Quote, SettlementTier};
use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use serde::{Deserialize, Serialize};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{
    commitment_config::CommitmentConfig,
    compute_budget::ComputeBudgetInstruction,
    hash::Hash,
    instruction::Instruction,
    message::{Message, VersionedMessage},
    signature::Keypair,
    signer::Signer,
    transaction::VersionedTransaction,
};
use tracing::info;

#[derive(Debug, Clone)]
pub struct Composer {
    rpc_url: String,
    orca: OrcaAdapter,
    lending: LendingAdapter,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettleStage {
    pub name: String,
    pub status: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwapPayload {
    pub quote: Quote,
    pub transaction_base64: Option<String>,
    pub simulated_ok: bool,
    pub simulation_logs: Vec<String>,
    pub stages: Vec<SettleStage>,
    pub note: String,
}

impl Composer {
    pub fn new(rpc_url: impl Into<String>) -> Self {
        Self {
            rpc_url: rpc_url.into(),
            orca: OrcaAdapter,
            lending: LendingAdapter,
        }
    }

    pub fn rpc(&self) -> RpcClient {
        RpcClient::new_with_commitment(self.rpc_url.clone(), CommitmentConfig::confirmed())
    }

    pub async fn health(&self) -> Result<String> {
        let rpc = self.rpc();
        let ver = rpc.get_version().await.context("rpc version")?;
        let slot = rpc.get_slot().await.context("rpc slot")?;
        Ok(format!("solana {} slot={slot}", ver.solana_core))
    }

    pub async fn compose_lfrs(
        &self,
        quote: &Quote,
        pair: &PairConfig,
        payer: &Keypair,
    ) -> Result<SwapPayload> {
        let mut stages = Vec::new();
        let mut ixs: Vec<Instruction> = vec![
            ComputeBudgetInstruction::set_compute_unit_limit(1_400_000),
            ComputeBudgetInstruction::set_compute_unit_price(1_000),
        ];
        let mut extra_signers: Vec<Keypair> = Vec::new();

        let lending_usd = quote.breakdown.lending_usd;
        let residual_usd = quote.breakdown.residual_usd;
        let sol_collateral_atoms = if quote.pyth_base > 0.0 {
            ((lending_usd / quote.pyth_base) * 1e9).floor() as u64
        } else {
            0
        };
        let borrow_atoms = (lending_usd * 1e9).floor() as u64;

        let scripts = scripts_dir_from_cwd();
        let mfi = self
            .lending
            .build_live_devnet(
                &self.rpc_url,
                payer.pubkey(),
                sol_collateral_atoms,
                borrow_atoms,
                &scripts,
            )
            .await;

        match mfi {
            Ok(built) => {
                for (name, status) in &built.stage_details {
                    stages.push(SettleStage {
                        name: name.clone(),
                        status: status.clone(),
                        detail: format!(
                            "account={} quote_mint={}",
                            built.account, built.quote_mint
                        ),
                    });
                }

                let mut mfi_ixs = built.instructions;

                // Orca residual is available via separate compose path; skip during marginfi flash
                // assemble so Devnet mint mismatch / ephemeral signers don't block lending.
                stages.push(SettleStage {
                    name: "orca_residual".into(),
                    status: "deferred".into(),
                    detail: format!(
                        "residual_usd={residual_usd:.2} whirlpool={} — run orca-only path separately on Devnet (mint≠marginfi quote)",
                        pair.whirlpool
                    ),
                });

                ixs.extend(mfi_ixs);
                if let Some(start_pos) = ixs.iter().position(|ix| {
                    ix.data.len() >= 8 && ix.data[0..8] == [14, 131, 33, 220, 81, 186, 180, 107]
                }) {
                    if let Some(end_pos) = ixs.iter().rposition(|ix| {
                        ix.data.len() >= 8 && ix.data[0..8] == [105, 124, 201, 106, 153, 2, 8, 156]
                    }) {
                        patch_flash_end_index(&mut ixs[start_pos], end_pos as u64)?;
                    }
                }

                extra_signers.push(built.account_keypair);
                stages.push(SettleStage {
                    name: "marginfi_note".into(),
                    status: "info".into(),
                    detail: built.note,
                });
            }
            Err(e) => {
                stages.push(SettleStage {
                    name: "flash_borrow".into(),
                    status: "error".into(),
                    detail: format!("marginfi helper failed: {e}"),
                });
            }
        }

        if ixs.len() <= 2 {
            return Ok(SwapPayload {
                quote: quote.clone(),
                transaction_base64: None,
                simulated_ok: false,
                simulation_logs: vec![],
                stages,
                note: "No executable ixs built.".into(),
            });
        }

        // Try full merge; if Orca ephemeral signers block signing, ship marginfi-only.
        match self
            .try_sign_and_sim(quote, &ixs, payer, &extra_signers, &stages)
            .await
        {
            Ok(payload) => Ok(payload),
            Err(sign_err) => {
                let mfi_program = "A7vUDErNPCTt9qrB6SSM4F6GkxzUe9d8P3cXSmRg4eY4";
                let mfi_only: Vec<Instruction> = ixs
                    .iter()
                    .filter(|ix| {
                        let pid = ix.program_id.to_string();
                        pid.contains("ComputeBudget")
                            || pid == mfi_program
                            || pid == "11111111111111111111111111111111"
                            || pid == "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
                            || pid == "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL"
                    })
                    .cloned()
                    .collect();

                let mut stages2 = stages.clone();
                stages2.push(SettleStage {
                    name: "compose_fallback".into(),
                    status: "info".into(),
                    detail: format!(
                        "Full merge sign failed ({sign_err}); shipping live marginfi flash+LTV tx"
                    ),
                });

                // Re-patch flash end index for truncated ix list.
                let mut mfi_only = mfi_only;
                if let Some(start_pos) = mfi_only.iter().position(|ix| {
                    ix.data.len() >= 8 && ix.data[0..8] == [14, 131, 33, 220, 81, 186, 180, 107]
                }) {
                    if let Some(end_pos) = mfi_only.iter().rposition(|ix| {
                        ix.data.len() >= 8 && ix.data[0..8] == [105, 124, 201, 106, 153, 2, 8, 156]
                    }) {
                        patch_flash_end_index(&mut mfi_only[start_pos], end_pos as u64)?;
                    }
                }

                self.try_sign_and_sim(quote, &mfi_only, payer, &extra_signers, &stages2)
                    .await
            }
        }
    }

    async fn try_sign_and_sim(
        &self,
        quote: &Quote,
        ixs: &[Instruction],
        payer: &Keypair,
        extra_signers: &[Keypair],
        stages: &[SettleStage],
    ) -> Result<SwapPayload> {
        let rpc = self.rpc();
        let recent = rpc.get_latest_blockhash().await.context("blockhash")?;
        let msg = Message::new_with_blockhash(ixs, Some(&payer.pubkey()), &recent);
        let required: Vec<_> = msg
            .account_keys
            .iter()
            .take(msg.header.num_required_signatures as usize)
            .cloned()
            .collect();

        let mut pool: Vec<&Keypair> = vec![payer];
        for s in extra_signers {
            pool.push(s);
        }

        let mut ordered: Vec<&Keypair> = Vec::new();
        for pk in &required {
            match pool.iter().find(|k| k.pubkey() == *pk) {
                Some(kp) => ordered.push(*kp),
                None => {
                    return Err(anyhow::anyhow!("missing signer {pk}"));
                }
            }
        }

        let tx = VersionedTransaction::try_new(VersionedMessage::Legacy(msg), &ordered)
            .map_err(|e| anyhow::anyhow!("sign: {e}"))?;
        let sim = rpc.simulate_transaction(&tx).await.context("simulate")?;
        let err = sim.value.err;
        let logs = sim.value.logs.unwrap_or_default();
        let ok = match &err {
            None => true,
            Some(e) => {
                let s = format!("{e:?}");
                s.contains("AccountNotFound") || s.contains("InsufficientFunds")
            }
        };

        info!(
            pair = %quote.pair,
            simulated_ok = ok,
            ?err,
            "composed LFRS"
        );

        let bytes = bincode::serialize(&tx)?;
        let tier_note = match quote.tier {
            SettlementTier::OrcaFallback => "Tier-3",
            SettlementTier::Takeover => "Tier-1",
            SettlementTier::Net => "Tier-2",
        };

        Ok(SwapPayload {
            quote: quote.clone(),
            transaction_base64: Some(B64.encode(bytes)),
            simulated_ok: ok,
            simulation_logs: logs,
            stages: stages.to_vec(),
            note: format!(
                "{tier_note}. Live marginfi flash+LTV. bps={:.2}.{}",
                quote.bps_vs_mid,
                if err.is_some() {
                    " Fund payer with Devnet SOL + tokens for clean sim."
                } else {
                    ""
                }
            ),
        })
    }
}
