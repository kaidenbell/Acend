use acend_core::{split_notional, PairConfig};
use anyhow::{anyhow, Context, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signature::Keypair,
};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::str::FromStr;
use tokio::process::Command;
use tracing::info;

/// marginfi mainnet program (reference). Devnet uses `dev` config via helper.
pub const MARGINFI_PROGRAM_ID: &str = "MFv2hWf31Z9kbCa1snEPYctwafyhdvnV7FZnsebVacA";
pub const MARGINFI_DEVNET_PROGRAM: &str = "A7vUDErNPCTt9qrB6SSM4F6GkxzUe9d8P3cXSmRg4eY4";
pub const MARGINFI_DEVNET_GROUP: &str = "52NC7T3NTPFFwoxJDFk9mbKcA7675DJ39H1iPNz5RjSV";

#[derive(Debug, Clone, Default)]
pub struct LendingAdapter;

#[derive(Debug, Clone)]
pub struct LendingQuote {
    pub lending_usd: f64,
    pub ltv_bps: u32,
    pub provider: &'static str,
}

#[derive(Debug, Clone)]
pub struct FlashLoanPlan {
    pub provider: &'static str,
    pub program_id: Pubkey,
    pub start_ix_name: &'static str,
    pub end_ix_name: &'static str,
    pub note: String,
}

#[derive(Debug, Deserialize)]
struct HelperIx {
    name: String,
    #[serde(rename = "programId")]
    program_id: String,
    data: String,
    keys: Vec<HelperKey>,
}

#[derive(Debug, Deserialize)]
struct HelperKey {
    pubkey: String,
    #[serde(rename = "isSigner")]
    is_signer: bool,
    #[serde(rename = "isWritable")]
    is_writable: bool,
}

#[derive(Debug, Deserialize)]
struct HelperOut {
    account: String,
    #[serde(rename = "accountSecret")]
    account_secret: String,
    prelude: Vec<HelperIx>,
    #[serde(rename = "startFlash")]
    start_flash: HelperIx,
    body: Vec<HelperIx>,
    #[serde(rename = "endFlash")]
    end_flash: HelperIx,
    note: String,
    #[serde(rename = "quoteMint")]
    quote_mint: String,
    #[serde(default, rename = "lookupTables")]
    lookup_tables: Vec<String>,
}

#[derive(Debug)]
pub struct MarginfiLiveBuild {
    pub account: Pubkey,
    pub account_keypair: Keypair,
    pub instructions: Vec<Instruction>,
    pub stage_details: Vec<(String, String)>,
    pub note: String,
    pub quote_mint: String,
    pub lookup_tables: Vec<String>,
}

impl LendingAdapter {
    pub fn quote(&self, pair: &PairConfig, notional_usd: f64) -> Result<LendingQuote> {
        let (lending, _) = split_notional(notional_usd, pair.ltv_bps);
        Ok(LendingQuote {
            lending_usd: lending,
            ltv_bps: pair.ltv_bps,
            provider: "marginfi_devnet_flash",
        })
    }

    pub fn flash_plan(&self) -> FlashLoanPlan {
        FlashLoanPlan {
            provider: "marginfi",
            program_id: Pubkey::from_str(MARGINFI_DEVNET_PROGRAM).expect("mfi"),
            start_ix_name: "lending_account_start_flashloan",
            end_ix_name: "lending_account_end_flashloan",
            note: "Devnet live flash via SDK helper".into(),
        }
    }

    pub fn program_id(&self) -> Result<Pubkey> {
        Pubkey::from_str(MARGINFI_DEVNET_PROGRAM).map_err(|e| anyhow!(e))
    }

    /// Build live marginfi init + flash + borrow + deposit via Node SDK helper.
    pub async fn build_live_devnet(
        &self,
        rpc_url: &str,
        authority: Pubkey,
        collateral_amount_atoms: u64,
        borrow_amount_atoms: u64,
        scripts_dir: &Path,
        mfi_env: &str,
        sell_base: bool,
    ) -> Result<MarginfiLiveBuild> {
        let mut script = scripts_dir.join("mfi-compose.js");
        if !script.exists() {
            script = scripts_dir.join("mfi-compose.mjs");
        }
        if !script.exists() {
            return Err(anyhow!(
                "missing mfi-compose.js in {} — run npm install in scripts/",
                scripts_dir.display()
            ));
        }

        let mut cmd = Command::new("node");
        cmd.arg(&script)
            .arg("--rpc")
            .arg(rpc_url)
            .arg("--authority")
            .arg(authority.to_string())
            .arg("--collateral-amount-atoms")
            .arg(collateral_amount_atoms.to_string())
            // legacy alias still accepted by the script
            .arg("--sol-amount-atoms")
            .arg(collateral_amount_atoms.to_string())
            .arg("--borrow-amount-atoms")
            .arg(borrow_amount_atoms.to_string())
            .arg("--sell-base")
            .arg(if sell_base { "true" } else { "false" })
            .arg("--env")
            .arg(mfi_env)
            .current_dir(scripts_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let out = cmd.output().await.context("spawn mfi-compose.mjs")?;
        if !out.status.success() {
            return Err(anyhow!(
                "mfi-compose failed: {}",
                String::from_utf8_lossy(&out.stderr)
            ));
        }

        let parsed: HelperOut =
            serde_json::from_slice(&out.stdout).context("parse mfi-compose json")?;
        let account = Pubkey::from_str(&parsed.account)?;
        let secret = B64
            .decode(parsed.account_secret.as_bytes())
            .context("account secret")?;
        let account_keypair = Keypair::try_from(secret.as_slice())
            .map_err(|e| anyhow!("account keypair: {e}"))?;

        let mut stage_details = Vec::new();
        let mut instructions: Vec<Instruction> = Vec::new();

        for ix in &parsed.prelude {
            instructions.push(helper_to_ix(ix)?);
            stage_details.push((ix.name.clone(), "built".into()));
        }

        let start_data = B64
            .decode(parsed.start_flash.data.as_bytes())
            .unwrap_or_default();
        let has_flash = start_data.len() >= 16;

        let start_idx_in_vec = instructions.len();
        if has_flash {
            instructions.push(helper_to_ix(&parsed.start_flash)?);
            stage_details.push(("lending_account_start_flashloan".into(), "built".into()));
        } else {
            stage_details.push((
                "lending_account_start_flashloan".into(),
                "skipped_devnet".into(),
            ));
        }

        for ix in &parsed.body {
            instructions.push(helper_to_ix(ix)?);
            stage_details.push((ix.name.clone(), "built".into()));
        }

        let end_idx_in_vec = instructions.len();
        let end_data = B64.decode(parsed.end_flash.data.as_bytes()).unwrap_or_default();
        if has_flash && end_data.len() >= 8 {
            instructions.push(helper_to_ix(&parsed.end_flash)?);
            stage_details.push(("lending_account_end_flashloan".into(), "built".into()));
            patch_flash_end_index(&mut instructions[start_idx_in_vec], end_idx_in_vec as u64)?;
        } else {
            stage_details.push((
                "lending_account_end_flashloan".into(),
                "skipped_devnet".into(),
            ));
        }

        info!(
            %account,
            ixs = instructions.len(),
            sell_base,
            "built live marginfi ixs"
        );

        Ok(MarginfiLiveBuild {
            account,
            account_keypair,
            instructions,
            stage_details,
            note: parsed.note,
            quote_mint: parsed.quote_mint,
            lookup_tables: parsed.lookup_tables,
        })
    }

    /// Anchor-style flash start (manual) once accounts known.
    pub fn start_flashloan_ix(
        &self,
        marginfi_account: Pubkey,
        signer: Pubkey,
        end_index: u64,
    ) -> Result<Instruction> {
        let program_id = Pubkey::from_str(MARGINFI_DEVNET_PROGRAM)?;
        let ixs_sysvar = solana_sdk::sysvar::instructions::id();
        let mut data = anchor_discriminator("lending_account_start_flashloan").to_vec();
        data.extend_from_slice(&end_index.to_le_bytes());
        Ok(Instruction {
            program_id,
            accounts: vec![
                AccountMeta::new(marginfi_account, false),
                AccountMeta::new_readonly(signer, true),
                AccountMeta::new_readonly(ixs_sysvar, false),
            ],
            data,
        })
    }

    pub fn end_flashloan_ix(&self, marginfi_account: Pubkey, signer: Pubkey) -> Result<Instruction> {
        let program_id = Pubkey::from_str(MARGINFI_DEVNET_PROGRAM)?;
        let data = anchor_discriminator("lending_account_end_flashloan").to_vec();
        Ok(Instruction {
            program_id,
            accounts: vec![
                AccountMeta::new(marginfi_account, false),
                AccountMeta::new_readonly(signer, true),
            ],
            data,
        })
    }
}

pub fn patch_flash_end_index(start_ix: &mut Instruction, end_index: u64) -> Result<()> {
    if start_ix.data.len() < 16 {
        return Err(anyhow!("start flash data too short"));
    }
    start_ix.data[8..16].copy_from_slice(&end_index.to_le_bytes());
    Ok(())
}

pub fn scripts_dir_from_cwd() -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    if cwd.join("scripts/mfi-compose.js").exists() || cwd.join("scripts/mfi-compose.mjs").exists()
    {
        cwd.join("scripts")
    } else if cwd.join("mfi-compose.js").exists() || cwd.join("mfi-compose.mjs").exists() {
        cwd
    } else {
        PathBuf::from("scripts")
    }
}

fn helper_to_ix(ix: &HelperIx) -> Result<Instruction> {
    Ok(Instruction {
        program_id: Pubkey::from_str(&ix.program_id)?,
        accounts: ix
            .keys
            .iter()
            .map(|k| {
                Ok(AccountMeta {
                    pubkey: Pubkey::from_str(&k.pubkey)?,
                    is_signer: k.is_signer,
                    is_writable: k.is_writable,
                })
            })
            .collect::<Result<Vec<_>>>()?,
        data: B64.decode(ix.data.as_bytes()).context("ix data")?,
    })
}

fn anchor_discriminator(name: &str) -> [u8; 8] {
    let preimage = format!("global:{name}");
    let hash = Sha256::digest(preimage.as_bytes());
    let mut out = [0u8; 8];
    out.copy_from_slice(&hash[..8]);
    out
}
