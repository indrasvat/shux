//! Append-only NDJSON audit log per plugin. See
//! `docs/designs/permissions/README.md` §5.4.
//!
//! - One file per plugin: `.shux/plugins/by-id/<uuid>/audit.log`.
//! - Rotation: at 1 MiB the current file is renamed to `audit.log.1`
//!   (existing `audit.log.{N}` shift up; `audit.log.5` is discarded).
//!   This is best-effort — failure to rotate logs a warning and the
//!   write proceeds against the un-rotated file.
//! - Symlink-rejecting (same TOCTOU guard as `grants.rs`).
//! - Synchronous; the audit log is on the hot path of every plugin
//!   RPC call. Writes are tiny (one JSON line). If this ever shows
//!   up in profiling, batch via a channel + dedicated writer task.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

const ROTATE_AT_BYTES: u64 = 1024 * 1024;
const KEEP_ROTATIONS: usize = 5;

#[derive(Debug, Serialize)]
pub struct AuditEntry<'a> {
    pub ts: String,
    pub plugin: &'a str,
    pub method: &'a str,
    pub params_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_pane: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_window: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_session: Option<&'a str>,
    pub decision: &'a str,
    pub reason: &'a str,
}

#[derive(thiserror::Error, Debug)]
pub enum AuditError {
    #[error("audit path contains a symlink (refusing): {0}")]
    Symlink(PathBuf),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialize error: {0}")]
    Serialize(#[from] serde_json::Error),
}

/// Compute a SHA-256 hex digest of the params JSON. Stored in audit
/// entries so duplicate calls can be deduplicated for forensic
/// review without storing the full params (which may be large or
/// contain key material — e.g. `pane.send_keys`).
pub fn params_hash(params: Option<&Value>) -> String {
    let mut h = Sha256::new();
    match params {
        Some(v) => {
            // Canonicalise via serde_json — keys ordered consistently.
            let bytes = serde_json::to_vec(v).unwrap_or_default();
            h.update(&bytes);
        }
        None => h.update(b"null"),
    }
    format!("sha256:{:x}", h.finalize())
}

/// Append one entry to `path`. Rotates if the existing file exceeds
/// 1 MiB before this write.
pub fn append(path: &Path, entry: &AuditEntry<'_>) -> Result<(), AuditError> {
    reject_symlinks(path)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Cheap rotate-before-write: stat the current file, rotate if
    // over the threshold, then open append. Race window between
    // rotate and write is harmless — the next call rotates again.
    if let Ok(meta) = std::fs::metadata(path)
        && meta.len() > ROTATE_AT_BYTES
    {
        let _ = rotate(path); // best-effort
    }

    let mut line = serde_json::to_vec(entry)?;
    line.push(b'\n');

    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    f.write_all(&line)?;
    f.flush()?;
    Ok(())
}

/// Shift `audit.log.{N}` → `audit.log.{N+1}` for N in 4..=1, then
/// `audit.log` → `audit.log.1`. Discards the oldest (`audit.log.5`).
fn rotate(path: &Path) -> std::io::Result<()> {
    let base = path.to_path_buf();
    for n in (1..KEEP_ROTATIONS).rev() {
        let from = base.with_extension(format!("log.{n}"));
        let to = base.with_extension(format!("log.{}", n + 1));
        if from.exists() {
            let _ = std::fs::rename(&from, &to);
        }
    }
    let dest = base.with_extension("log.1");
    std::fs::rename(&base, &dest)
}

fn reject_symlinks(path: &Path) -> Result<(), AuditError> {
    if let Ok(meta) = std::fs::symlink_metadata(path)
        && meta.file_type().is_symlink()
    {
        return Err(AuditError::Symlink(path.to_path_buf()));
    }
    Ok(())
}

/// ISO 8601 timestamp in UTC, millisecond precision. Used as the
/// `ts` field in every audit entry.
pub fn iso_now() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn append_creates_file_and_writes_ndjson() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("audit.log");
        let entry = AuditEntry {
            ts: iso_now(),
            plugin: "watcher",
            method: "pane.snapshot",
            params_hash: params_hash(Some(&serde_json::json!({"pane_id": "abc"}))),
            target_pane: Some("abc"),
            target_window: None,
            target_session: None,
            decision: "allow",
            reason: "owned_by_plugin",
        };
        append(&path, &entry).unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.ends_with('\n'));
        let parsed: serde_json::Value = serde_json::from_str(contents.trim()).unwrap();
        assert_eq!(parsed["plugin"], "watcher");
        assert_eq!(parsed["method"], "pane.snapshot");
        assert_eq!(parsed["decision"], "allow");
        assert_eq!(parsed["target_pane"], "abc");
    }

    #[test]
    fn params_hash_stable_and_distinct() {
        let a = params_hash(Some(&serde_json::json!({"x": 1, "y": 2})));
        let b = params_hash(Some(&serde_json::json!({"x": 1, "y": 2})));
        let c = params_hash(Some(&serde_json::json!({"x": 1, "y": 3})));
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert!(a.starts_with("sha256:"));
    }

    #[test]
    fn rotation_shifts_files() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("audit.log");
        std::fs::write(&path, vec![b'x'; (ROTATE_AT_BYTES as usize) + 1]).unwrap();

        let entry = AuditEntry {
            ts: iso_now(),
            plugin: "p",
            method: "m",
            params_hash: "sha256:0".into(),
            target_pane: None,
            target_window: None,
            target_session: None,
            decision: "allow",
            reason: "test",
        };
        append(&path, &entry).unwrap();

        assert!(dir.path().join("audit.log.1").exists());
        // The new audit.log only has the one new line; the rotated
        // file holds the original blob of x's.
        let new_size = std::fs::metadata(&path).unwrap().len();
        assert!(new_size < 1024);
    }
}
