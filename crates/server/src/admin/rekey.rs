//! Master-key rotation: one atomic row at a time, counts only on stdout.

use std::io::Write;

use anyhow::Context as _;
use clap::Args;
use shelfy_core::db::ControlDb;

use crate::ai::vault::KeyVault;
use crate::config::{DataDir, VaultArgs};
use crate::control::provider_keys;

#[derive(Debug, Args)]
pub struct RekeyArgs {
    /// Authenticate all rows and report what would change without writing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub vault: VaultArgs,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RekeyReport {
    pub scanned: u64,
    pub rekeyed: u64,
    pub unchanged: u64,
    pub skipped: u64,
    pub failed: u64,
}

pub fn run(data: &DataDir, args: RekeyArgs, out: &mut dyn Write) -> anyhow::Result<()> {
    let vault = args.vault.into_vault().map_err(anyhow::Error::msg)?;
    let db = super::open_existing_control(data)?;
    let report = rekey(&db, &vault, args.dry_run)?;
    writeln!(
        out,
        "scanned={} rekeyed={} unchanged={} skipped={} failed={}",
        report.scanned, report.rekeyed, report.unchanged, report.skipped, report.failed
    )?;
    if report.failed != 0 {
        anyhow::bail!("provider key rotation incomplete; failed rows were kept")
    }
    if report.skipped != 0 {
        anyhow::bail!("provider keys changed during rotation; run rekey again")
    }
    Ok(())
}

/// Re-running skips current rows. Failures leave their old row intact, and other
/// rows still progress. Concurrent replacement is never overwritten.
pub fn rekey(db: &ControlDb, vault: &KeyVault, dry_run: bool) -> anyhow::Result<RekeyReport> {
    let current = vault
        .key_version()
        .context("the provider key vault is disabled")?;
    let mut report = RekeyReport::default();
    let mut cursor: Option<(String, String)> = None;
    loop {
        let rows = db.read(|conn| {
            provider_keys::batch(conn, cursor.as_ref().map(|(u, p)| (u.as_str(), p.as_str())))
        })?;
        if rows.is_empty() {
            break;
        }
        for row in rows {
            cursor = Some((row.user_id.clone(), row.provider_id.clone()));
            report.scanned += 1;
            let Ok(plaintext) = vault.open(&row.user_id, &row.provider_id, &row.sealed) else {
                report.failed += 1;
                continue;
            };
            if row.sealed.key_version == current {
                report.unchanged += 1;
                continue;
            }
            if dry_run {
                report.rekeyed += 1;
                continue;
            }
            let Ok(sealed) = vault.seal(&row.user_id, &row.provider_id, &plaintext) else {
                report.failed += 1;
                continue;
            };
            if db.write(|tx| provider_keys::replace_sealed(tx, &row, &sealed))? {
                report.rekeyed += 1;
            } else {
                report.skipped += 1
            }
        }
    }
    Ok(report)
}
