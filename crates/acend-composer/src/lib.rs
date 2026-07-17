use acend_adapters::{
    patch_flash_end_index, scripts_dir_from_cwd, LendingAdapter, OrcaAdapter,
};
use acend_core::{PairConfig, Quote, SettlementTier};
use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use serde::{Deserialize, Serialize};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{
    address_lookup_table::state::AddressLookupTable,
    commitment_config::CommitmentConfig,
    compute_budget::ComputeBudgetInstruction,
    hash::Hash,
    instruction::Instruction,
    message::{v0, AddressLookupTableAccount, Message, VersionedMessage},
    pubkey::Pubkey,
    signature::Keypair,
    signer::{null_signer::NullSigner, Signer},
    transaction::VersionedTransaction,
};
use std::str::FromStr;
use tracing::info;

fn pair_is_mainnet(pair: &PairConfig) -> bool {
    pair.quote_mint == "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"
        && !pair.whirlpool.trim().is_empty()
}

fn mints_aligned(pair: &PairConfig, quote_mint: &str) -> bool {
    pair.quote_mint == quote_mint && !pair.whirlpool.trim().is_empty()
}

#[derive(Debug, Clone)]
pub struct Composer {
    rpc_url: String,
    orca: OrcaAdapter,
    lending: LendingAdapter,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ComposeOpts {
    pub send: bool,
    /// When true, `simulated_ok` is only true if simulation has no error.
    pub strict: bool,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payer: Option<String>,
    /// True when fee-payer slot is unsigned (NullSigner) — client must sign before send.
    #[serde(default)]
    pub needs_client_signature: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recent_blockhash: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lookup_tables: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sol_lamports_before: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sol_lamports_after: Option<u64>,
}

struct BuiltIxs {
    ixs: Vec<Instruction>,
    extra_signers: Vec<Keypair>,
    stages: Vec<SettleStage>,
    lookup_tables: Vec<String>,
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

    pub async fn lamports(&self, pubkey: &Pubkey) -> Result<u64> {
        Ok(self.rpc().get_balance(pubkey).await.unwrap_or(0))
    }

    /// Marginfi flash+LTV path — fully signed by `payer` (CLI / send path).
    pub async fn compose_lfrs(
        &self,
        quote: &Quote,
        pair: &PairConfig,
        payer: &Keypair,
        send: bool,
    ) -> Result<SwapPayload> {
        self.compose_lfrs_inner(quote, pair, PayerMode::Keypair(payer), ComposeOpts { send, strict: false })
            .await
    }

    /// Same path, fee-payer left for the client wallet to sign.
    pub async fn compose_lfrs_for_payer(
        &self,
        quote: &Quote,
        pair: &PairConfig,
        payer: Pubkey,
        opts: ComposeOpts,
    ) -> Result<SwapPayload> {
        self.compose_lfrs_inner(quote, pair, PayerMode::Client(payer), opts)
            .await
    }

    /// Separate Orca residual tx — fully signed by `payer`.
    pub async fn compose_orca_residual(
        &self,
        quote: &Quote,
        pair: &PairConfig,
        payer: &Keypair,
        send: bool,
    ) -> Result<SwapPayload> {
        self.compose_orca_inner(quote, pair, PayerMode::Keypair(payer), ComposeOpts { send, strict: false })
            .await
    }

    pub async fn compose_orca_residual_for_payer(
        &self,
        quote: &Quote,
        pair: &PairConfig,
        payer: Pubkey,
        opts: ComposeOpts,
    ) -> Result<SwapPayload> {
        self.compose_orca_inner(quote, pair, PayerMode::Client(payer), opts)
            .await
    }

    async fn compose_lfrs_inner(
        &self,
        quote: &Quote,
        pair: &PairConfig,
        payer: PayerMode<'_>,
        opts: ComposeOpts,
    ) -> Result<SwapPayload> {
        let authority = payer.pubkey();
        let built = self.build_lfrs_ixs(quote, pair, authority).await?;

        if built.ixs.len() <= 2 {
            return Ok(empty_payload(
                quote,
                built.stages,
                "No executable ixs built.".into(),
                authority,
            ));
        }

        match self
            .finalize(quote, &built.ixs, &payer, &built.extra_signers, &built.stages, &built.lookup_tables, opts)
            .await
        {
            Ok(payload) => Ok(payload),
            Err(sign_err) => {
                let mfi_program_dev = "A7vUDErNPCTt9qrB6SSM4F6GkxzUe9d8P3cXSmRg4eY4";
                let mfi_program_main = "MFv2hWf31Z9kbCa1snEPYctwafyhdvnV7FZnsebVacA";
                let mut mfi_only: Vec<Instruction> = built
                    .ixs
                    .iter()
                    .filter(|ix| {
                        let pid = ix.program_id.to_string();
                        pid.contains("ComputeBudget")
                            || pid == mfi_program_dev
                            || pid == mfi_program_main
                            || pid == "11111111111111111111111111111111"
                            || pid == "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
                            || pid == "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL"
                    })
                    .cloned()
                    .collect();

                let mut stages2 = built.stages.clone();
                stages2.push(SettleStage {
                    name: "compose_fallback".into(),
                    status: "info".into(),
                    detail: format!(
                        "Full merge sign failed ({sign_err}); shipping live marginfi flash+LTV tx"
                    ),
                });

                if let Some(start_pos) = mfi_only.iter().position(|ix| {
                    ix.data.len() >= 8 && ix.data[0..8] == [14, 131, 33, 220, 81, 186, 180, 107]
                }) {
                    if let Some(end_pos) = mfi_only.iter().rposition(|ix| {
                        ix.data.len() >= 8 && ix.data[0..8] == [105, 124, 201, 106, 153, 2, 8, 156]
                    }) {
                        patch_flash_end_index(&mut mfi_only[start_pos], end_pos as u64)?;
                    }
                }

                self.finalize(
                    quote,
                    &mfi_only,
                    &payer,
                    &built.extra_signers,
                    &stages2,
                    &built.lookup_tables,
                    opts,
                )
                .await
            }
        }
    }

    async fn compose_orca_inner(
        &self,
        quote: &Quote,
        pair: &PairConfig,
        payer: PayerMode<'_>,
        opts: ComposeOpts,
    ) -> Result<SwapPayload> {
        let authority = payer.pubkey();
        let mut stages = Vec::new();
        let residual_usd = quote.breakdown.residual_usd;
        let mut ixs: Vec<Instruction> = vec![
            ComputeBudgetInstruction::set_compute_unit_limit(1_400_000),
            ComputeBudgetInstruction::set_compute_unit_price(1_000),
        ];
        let mut extra_signers: Vec<Keypair> = Vec::new();

        match self
            .orca
            .build_residual_swap(
                &self.rpc_url,
                pair,
                residual_usd,
                quote.pyth_base,
                authority,
                50,
            )
            .await
        {
            Ok(built) => {
                stages.push(SettleStage {
                    name: "orca_residual".into(),
                    status: "built".into(),
                    detail: format!(
                        "atoms={} whirlpool={} {}",
                        built.input_amount_atoms, built.whirlpool, built.estimated_out_note
                    ),
                });
                ixs.extend(built.instructions);
                extra_signers.extend(built.additional_signers);
            }
            Err(e) => {
                stages.push(SettleStage {
                    name: "orca_residual".into(),
                    status: "error".into(),
                    detail: e.to_string(),
                });
                return Ok(empty_payload(
                    quote,
                    stages,
                    format!("Orca residual build failed: {e}"),
                    authority,
                ));
            }
        }

        self.finalize(quote, &ixs, &payer, &extra_signers, &stages, &[], opts)
            .await
    }

    async fn build_lfrs_ixs(
        &self,
        quote: &Quote,
        pair: &PairConfig,
        authority: Pubkey,
    ) -> Result<BuiltIxs> {
        let mut stages = Vec::new();
        let mut ixs: Vec<Instruction> = vec![
            ComputeBudgetInstruction::set_compute_unit_limit(1_400_000),
            ComputeBudgetInstruction::set_compute_unit_price(1_000),
        ];
        let mut extra_signers: Vec<Keypair> = Vec::new();
        let mut lookup_tables = Vec::new();

        let lending_usd = quote.breakdown.lending_usd;
        let residual_usd = quote.breakdown.residual_usd;
        let sol_collateral_atoms = if quote.pyth_base > 0.0 {
            ((lending_usd / quote.pyth_base) * 1e9).floor() as u64
        } else {
            0
        };
        let borrow_atoms = (lending_usd * 10f64.powi(pair.quote_decimals as i32)).floor() as u64;

        let scripts = scripts_dir_from_cwd();
        let mfi = self
            .lending
            .build_live_devnet(
                &self.rpc_url,
                authority,
                sol_collateral_atoms,
                borrow_atoms,
                &scripts,
                if pair_is_mainnet(pair) {
                    "production"
                } else {
                    "dev"
                },
            )
            .await;

        match mfi {
            Ok(built) => {
                lookup_tables = built.lookup_tables.clone();
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

                if mints_aligned(pair, &built.quote_mint) && residual_usd > 0.0 {
                    match self
                        .orca
                        .build_residual_swap(
                            &self.rpc_url,
                            pair,
                            residual_usd,
                            quote.pyth_base,
                            authority,
                            50,
                        )
                        .await
                    {
                        Ok(orca) => {
                            if let Some(end_pos) = mfi_ixs.iter().rposition(|ix| {
                                ix.data.len() >= 8
                                    && ix.data[0..8] == [105, 124, 201, 106, 153, 2, 8, 156]
                            }) {
                                for (i, oix) in orca.instructions.into_iter().enumerate() {
                                    mfi_ixs.insert(end_pos + i, oix);
                                }
                                extra_signers.extend(orca.additional_signers);
                                stages.push(SettleStage {
                                    name: "orca_residual".into(),
                                    status: "spliced".into(),
                                    detail: format!(
                                        "residual_usd={residual_usd:.2} atoms={} whirlpool={}",
                                        orca.input_amount_atoms, orca.whirlpool
                                    ),
                                });
                            } else {
                                stages.push(SettleStage {
                                    name: "orca_residual".into(),
                                    status: "built_no_flash_end".into(),
                                    detail: orca.estimated_out_note,
                                });
                                mfi_ixs.extend(orca.instructions);
                                extra_signers.extend(orca.additional_signers);
                            }
                        }
                        Err(e) => {
                            stages.push(SettleStage {
                                name: "orca_residual".into(),
                                status: "error".into(),
                                detail: e.to_string(),
                            });
                        }
                    }
                } else {
                    stages.push(SettleStage {
                        name: "orca_residual".into(),
                        status: "deferred".into(),
                        detail: format!(
                            "residual_usd={residual_usd:.2} whirlpool={} (mint mismatch or empty pool)",
                            pair.whirlpool
                        ),
                    });
                }

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

        Ok(BuiltIxs {
            ixs,
            extra_signers,
            stages,
            lookup_tables,
        })
    }

    async fn load_alts(&self, addrs: &[String]) -> Vec<AddressLookupTableAccount> {
        if addrs.is_empty() {
            return Vec::new();
        }
        let rpc = self.rpc();
        let mut out = Vec::new();
        for s in addrs {
            let Ok(pk) = Pubkey::from_str(s) else {
                continue;
            };
            let Ok(acc) = rpc.get_account(&pk).await else {
                continue;
            };
            match AddressLookupTable::deserialize(&acc.data) {
                Ok(table) => {
                    out.push(AddressLookupTableAccount {
                        key: pk,
                        addresses: table.addresses.to_vec(),
                    });
                }
                Err(e) => {
                    info!(%pk, err = %e, "skip bad ALT");
                }
            }
        }
        out
    }

    async fn finalize(
        &self,
        quote: &Quote,
        ixs: &[Instruction],
        payer: &PayerMode<'_>,
        extra_signers: &[Keypair],
        stages: &[SettleStage],
        lookup_table_addrs: &[String],
        opts: ComposeOpts,
    ) -> Result<SwapPayload> {
        let rpc = self.rpc();
        let authority = payer.pubkey();
        let before = rpc.get_balance(&authority).await.unwrap_or(0);
        let recent = rpc.get_latest_blockhash().await.context("blockhash")?;
        let alts = self.load_alts(lookup_table_addrs).await;

        let versioned_msg = if alts.is_empty() {
            VersionedMessage::Legacy(Message::new_with_blockhash(
                ixs,
                Some(&authority),
                &recent,
            ))
        } else {
            match v0::Message::try_compile(&authority, ixs, &alts, recent) {
                Ok(m) => VersionedMessage::V0(m),
                Err(e) => {
                    info!(%e, "v0 compile failed; falling back to legacy");
                    VersionedMessage::Legacy(Message::new_with_blockhash(
                        ixs,
                        Some(&authority),
                        &recent,
                    ))
                }
            }
        };

        let required: Vec<Pubkey> = match &versioned_msg {
            VersionedMessage::Legacy(m) => m
                .account_keys
                .iter()
                .take(m.header.num_required_signatures as usize)
                .cloned()
                .collect(),
            VersionedMessage::V0(m) => m
                .account_keys
                .iter()
                .take(m.header.num_required_signatures as usize)
                .cloned()
                .collect(),
        };

        // Build + sign in a sync block so no &dyn Signer lives across .await (Send for axum).
        let tx = match payer {
            PayerMode::Keypair(kp) => {
                let mut ordered: Vec<&Keypair> = Vec::new();
                for pk in &required {
                    if *pk == kp.pubkey() {
                        ordered.push(*kp);
                    } else if let Some(extra) = extra_signers.iter().find(|s| s.pubkey() == *pk) {
                        ordered.push(extra);
                    } else {
                        return Err(anyhow::anyhow!("missing signer {pk}"));
                    }
                }
                VersionedTransaction::try_new(versioned_msg, &ordered)
                    .map_err(|e| anyhow::anyhow!("sign: {e}"))?
            }
            PayerMode::Client(pk) => {
                let null_payer = NullSigner::new(pk);
                let mut ordered: Vec<&dyn Signer> = Vec::new();
                for req in &required {
                    if *req == *pk {
                        ordered.push(&null_payer);
                    } else if let Some(extra) = extra_signers.iter().find(|s| s.pubkey() == *req)
                    {
                        ordered.push(extra);
                    } else {
                        return Err(anyhow::anyhow!("missing signer {req}"));
                    }
                }
                VersionedTransaction::try_new(versioned_msg, &ordered)
                    .map_err(|e| anyhow::anyhow!("sign: {e}"))?
            }
        };

        let sim = rpc.simulate_transaction(&tx).await.context("simulate")?;
        let err = sim.value.err;
        let logs = sim.value.logs.unwrap_or_default();
        let clean_ok = err.is_none();
        let soft_ok = match &err {
            None => true,
            Some(e) => {
                let s = format!("{e:?}");
                s.contains("AccountNotFound") || s.contains("InsufficientFunds")
            }
        };
        let ok = if opts.strict { clean_ok } else { soft_ok };
        let needs_client = matches!(payer, PayerMode::Client(_));

        info!(
            pair = %quote.pair,
            simulated_ok = ok,
            clean_ok,
            strict = opts.strict,
            needs_client,
            alts = alts.len(),
            ?err,
            "composed LFRS"
        );

        let mut signature = None;
        let mut after = before;
        let mut send_note = String::new();
        if opts.send {
            if needs_client {
                send_note = " Send skipped (needs client signature).".into();
            } else if clean_ok {
                match rpc.send_and_confirm_transaction(&tx).await {
                    Ok(sig) => {
                        signature = Some(sig.to_string());
                        tokio::time::sleep(std::time::Duration::from_millis(800)).await;
                        after = rpc.get_balance(&authority).await.unwrap_or(before);
                        send_note = format!(" Sent: {sig}.");
                        info!(%sig, before, after, "LFRS sent");
                    }
                    Err(e) => send_note = format!(" Send failed: {e}."),
                }
            } else {
                send_note = format!(" Send skipped (sim err: {:?}). Fund payer and retry.", err);
            }
        }

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
                "{tier_note}. Live path. bps={:.2}.{}{}{}",
                quote.bps_vs_mid,
                if needs_client {
                    " Partially signed - client must sign fee-payer before send."
                } else if !clean_ok {
                    " Fund payer with SOL + tokens for clean sim."
                } else {
                    ""
                },
                if !alts.is_empty() {
                    format!(" ALTs={}", alts.len())
                } else {
                    String::new()
                },
                send_note
            ),
            signature,
            payer: Some(authority.to_string()),
            needs_client_signature: needs_client,
            recent_blockhash: Some(hash_str(recent)),
            lookup_tables: lookup_table_addrs.to_vec(),
            sol_lamports_before: Some(before),
            sol_lamports_after: if opts.send { Some(after) } else { None },
        })
    }
}

fn hash_str(h: Hash) -> String {
    h.to_string()
}

fn empty_payload(
    quote: &Quote,
    stages: Vec<SettleStage>,
    note: String,
    payer: Pubkey,
) -> SwapPayload {
    SwapPayload {
        quote: quote.clone(),
        transaction_base64: None,
        simulated_ok: false,
        simulation_logs: vec![],
        stages,
        note,
        signature: None,
        payer: Some(payer.to_string()),
        needs_client_signature: false,
        recent_blockhash: None,
        lookup_tables: vec![],
        sol_lamports_before: None,
        sol_lamports_after: None,
    }
}

enum PayerMode<'a> {
    Keypair(&'a Keypair),
    Client(Pubkey),
}

impl PayerMode<'_> {
    fn pubkey(&self) -> Pubkey {
        match self {
            PayerMode::Keypair(k) => k.pubkey(),
            PayerMode::Client(p) => *p,
        }
    }
}
