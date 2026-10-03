//! Streaming ZIP64 writer. The snapshot, not the changing live DB, names the masters.
use rusqlite::Connection;
use rusqlite::backup::{Backup, StepResult};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use shelfy_media::store::MediaStore;
use shelfy_media::{Digest, MediaKind};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

/// Manifest metadata; hashes cover the uncompressed payload (not the manifest itself).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Manifest {
    pub format: String,
    pub version: u32,
    pub created_at: i64,
    pub server_version: String,
    pub library_schema_version: usize,
    pub counts: BTreeMap<String, u64>,
    pub entries: Vec<Entry>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Entry {
    pub path: String,
    pub sha256: String,
    pub bytes: u64,
}

fn check(token: &CancellationToken, heartbeat: &impl Fn()) -> anyhow::Result<()> {
    heartbeat();
    anyhow::ensure!(!token.is_cancelled(), "cancelled");
    Ok(())
}
/// Online backup on the caller's fixed source read transaction (`UserDb::read`).
/// Checks cancellation every 128 pages.
pub fn snapshot(
    source: &Connection,
    target: &Path,
    token: &CancellationToken,
    heartbeat: &impl Fn(),
) -> anyhow::Result<()> {
    drop(create_private_file(target)?);
    let mut dest = Connection::open(target)?;
    let result = (|| -> anyhow::Result<()> {
        // Establish a single WAL snapshot before incremental backup begins.
        source.query_row("SELECT count(*) FROM sqlite_schema", [], |r| {
            r.get::<_, i64>(0)
        })?;
        let backup = Backup::new(source, &mut dest)?;
        loop {
            check(token, heartbeat)?;
            match backup.step(128)? {
                StepResult::Done => break,
                StepResult::More => {}
                StepResult::Busy | StepResult::Locked => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                _ => anyhow::bail!("unexpected backup result"),
            }
        }
        Ok(())
    })();
    result?;
    dest.pragma_update(None, "journal_mode", "DELETE")?;
    let integrity: String = dest.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
    anyhow::ensure!(integrity == "ok", "snapshot integrity failed");
    drop(dest);
    File::open(target)?.sync_all()?;
    Ok(())
}
fn create_private_file(path: &Path) -> std::io::Result<File> {
    let mut options = fs::OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options.open(path)
}

fn copy_entry(
    writer: &mut ZipWriter<File>,
    path: &Path,
    name: &str,
    method: CompressionMethod,
    token: &CancellationToken,
    heartbeat: &impl Fn(),
) -> anyhow::Result<Entry> {
    writer.start_file(
        name,
        SimpleFileOptions::default()
            .compression_method(method)
            .large_file(true)
            .unix_permissions(0o600),
    )?;
    anyhow::ensure!(
        fs::symlink_metadata(path)?.is_file(),
        "object is not a regular file"
    );
    let mut input = File::open(path)?;
    let limit = input.metadata()?.len();
    anyhow::ensure!(input.metadata()?.is_file(), "object is not a regular file");
    let mut buffer = [0u8; 64 * 1024];
    let mut hash = Sha256::new();
    let mut bytes = 0u64;
    loop {
        check(token, heartbeat)?;
        let n = input.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        writer.write_all(&buffer[..n])?;
        hash.update(&buffer[..n]);
        bytes += n as u64;
        anyhow::ensure!(bytes <= limit, "object grew while exporting");
    }
    Ok(Entry {
        path: name.to_owned(),
        sha256: Digest::from_bytes(hash.finalize().into()).to_string(),
        bytes,
    })
}
/// Writes an archive to `part`, leaving publication to its caller. Metadata is spooled
/// to disk; only ZIP directory metadata grows with the entry count, never payloads.
pub struct Build<'a> {
    pub snapshot: &'a Path,
    pub users: &'a Path,
    pub user: &'a str,
    pub part: &'a Path,
    pub entries_path: &'a Path,
    pub created_at: i64,
}

pub fn write(
    build: Build<'_>,
    token: &CancellationToken,
    heartbeat: impl Fn(),
) -> anyhow::Result<u64> {
    let Build {
        snapshot,
        users,
        user,
        part,
        entries_path,
        created_at,
    } = build;
    let db = Connection::open_with_flags(snapshot, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut counts = BTreeMap::new();
    let tables = db.prepare("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name NOT IN (SELECT name FROM pragma_table_list WHERE type='shadow') ORDER BY name")?
        .query_map([], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    for table in tables {
        let escaped = table.replace('"', "\"\"");
        counts.insert(
            table,
            db.query_row(&format!("SELECT count(*) FROM \"{escaped}\""), [], |r| {
                r.get::<_, i64>(0).map(|n| n.max(0) as u64)
            })?,
        );
    }
    let media = MediaStore::new(users).user(user)?;
    let mut zip = ZipWriter::new(create_private_file(part)?);
    zip.set_raw_zip64_extensible_data_sector(Box::new([]));
    let mut entries = create_private_file(entries_path)?;
    let first = copy_entry(
        &mut zip,
        snapshot,
        "library.sqlite",
        CompressionMethod::Deflated,
        token,
        &heartbeat,
    )?;
    serde_json::to_writer(&mut entries, &first)?;
    let mut stmt = db.prepare("SELECT sha256, ext, bytes FROM media_objects ORDER BY id")?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let raw: Vec<u8> = row.get(0)?;
        let digest =
            Digest::from_slice(&raw).ok_or_else(|| anyhow::anyhow!("invalid object digest"))?;
        let ext: String = row.get(1)?;
        let kind =
            MediaKind::from_ext(&ext).ok_or_else(|| anyhow::anyhow!("invalid object extension"))?;
        let size = row.get::<_, i64>(2)?.max(0) as u64;
        let name = format!("media/{}/{}.{}", digest.shard(), digest, ext);
        anyhow::ensure!(
            fs::metadata(media.object_path(&digest, kind))?.len() == size,
            "object size mismatch"
        );
        let entry = copy_entry(
            &mut zip,
            &media.object_path(&digest, kind),
            &name,
            CompressionMethod::Stored,
            token,
            &heartbeat,
        )?;
        anyhow::ensure!(
            entry.sha256 == digest.to_string() && entry.bytes == size,
            "object integrity failed"
        );
        entries.write_all(b",")?;
        serde_json::to_writer(&mut entries, &entry)?;
    }
    entries.sync_all()?;
    drop(entries);
    let metadata = Manifest {
        format: "shelfy-export".to_owned(),
        version: 2,
        created_at,
        server_version: env!("CARGO_PKG_VERSION").to_owned(),
        library_schema_version: shelfy_core::schema::version(&db)?,
        counts,
        entries: vec![],
    };
    let prefix = serde_json::to_vec(&metadata)?;
    // `entries` is the last serialized field; append its streamed contents.
    let prefix = prefix
        .strip_suffix(b"[]}")
        .ok_or_else(|| anyhow::anyhow!("manifest layout"))?;
    zip.start_file(
        "manifest.json",
        SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .large_file(true)
            .unix_permissions(0o600),
    )?;
    zip.write_all(prefix)?;
    zip.write_all(b"[")?;
    let mut entries = File::open(entries_path)?;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        check(token, &heartbeat)?;
        let n = entries.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        zip.write_all(&buffer[..n])?;
    }
    zip.write_all(b"]}")?;
    let output = zip.finish()?;
    output.sync_all()?;
    Ok(fs::metadata(part)?.len())
}
