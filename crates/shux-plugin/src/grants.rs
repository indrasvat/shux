//! Per-plugin grant store — TOML-backed, atomic writes,
//! symlink-rejecting. See `docs/designs/permissions/README.md` §5.3.
//!
//! Grant file layout (`.shux/plugins/by-id/<uuid>/grants.toml`):
//!
//! ```toml
//! [grants]
//! "pane.snapshot"  = "*"
//! "pane.send_keys" = ["a1b2-...", "c3d4-..."]
//!
//! [subscribes]
//! allowed = ["pane.exited", "plugin.foo.bar"]
//! ```
//!
//! `"*"` = any target. A list = only those target IDs. Methods with
//! no entry default to denied (per default-deny model).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Grants {
    #[serde(default)]
    pub grants: BTreeMap<String, Scope>,
    #[serde(default)]
    pub subscribes: SubscribesSection,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SubscribesSection {
    #[serde(default)]
    pub allowed: Vec<String>,
}

/// A grant scope: blanket `Any` or a list of explicit target IDs
/// (UUID strings — we don't type these to a specific entity type
/// because the same scope is reused for pane/window/session ids).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Scope {
    Any(StarMarker),
    Targets(Vec<String>),
}

/// Newtype that serializes as the literal string `"*"`. Lets the TOML
/// deserializer pick the right `Scope` variant via `untagged`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StarMarker;

impl Serialize for StarMarker {
    fn serialize<S>(&self, ser: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        ser.serialize_str("*")
    }
}

impl<'de> Deserialize<'de> for StarMarker {
    fn deserialize<D>(de: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(de)?;
        if s == "*" {
            Ok(StarMarker)
        } else {
            Err(serde::de::Error::custom(format!(
                "expected \"*\" for blanket grant scope, got {s:?}"
            )))
        }
    }
}

impl Scope {
    pub fn is_any(&self) -> bool {
        matches!(self, Scope::Any(_))
    }

    /// Does this scope cover `target_id`? Blanket `*` always yes;
    /// `Targets` matches only on exact UUID-string membership.
    pub fn covers(&self, target_id: &str) -> bool {
        match self {
            Scope::Any(_) => true,
            Scope::Targets(ids) => ids.iter().any(|t| t == target_id),
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub enum GrantsError {
    #[error("grants path is a symlink (refusing): {0}")]
    Symlink(PathBuf),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("parse error: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("serialize error: {0}")]
    Serialize(#[from] toml::ser::Error),
}

impl Grants {
    /// Load grants from disk. Returns an empty `Grants` if the file
    /// doesn't exist (default-deny default — no entries = nothing
    /// allowed). Rejects symlinked paths.
    pub fn load(path: &Path) -> Result<Self, GrantsError> {
        reject_symlinks(path)?;
        match std::fs::read_to_string(path) {
            Ok(s) => Ok(toml::from_str(&s)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    /// Atomic write: serialise → temp file in same dir → `rename(2)`.
    /// Mirrors the pattern used for plugin state (lib.rs:837).
    pub fn save(&self, path: &Path) -> Result<(), GrantsError> {
        reject_symlinks(path)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let body = toml::to_string_pretty(self)?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, body)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Add a method grant. `target_id` is `None` for blanket grants
    /// (sets `Scope::Any`); `Some(id)` appends to a targets list,
    /// upgrading an existing list. If the method already has a
    /// blanket `*` grant, adding a target is a no-op.
    pub fn add(&mut self, method: &str, target_id: Option<&str>) {
        match target_id {
            None => {
                self.grants
                    .insert(method.to_string(), Scope::Any(StarMarker));
            }
            Some(tid) => {
                let entry = self
                    .grants
                    .entry(method.to_string())
                    .or_insert_with(|| Scope::Targets(Vec::new()));
                match entry {
                    Scope::Any(_) => {}
                    Scope::Targets(list) => {
                        if !list.iter().any(|t| t == tid) {
                            list.push(tid.to_string());
                        }
                    }
                }
            }
        }
    }

    /// Remove a method grant. `target_id == None` drops the entire
    /// entry; `Some(id)` removes that target. Removing the last
    /// target drops the entry; removing a target from an `Any`
    /// scope is a no-op (use `None` to drop the blanket grant).
    pub fn remove(&mut self, method: &str, target_id: Option<&str>) {
        match target_id {
            None => {
                self.grants.remove(method);
            }
            Some(tid) => {
                if let Some(Scope::Targets(list)) = self.grants.get_mut(method) {
                    list.retain(|t| t != tid);
                    if list.is_empty() {
                        self.grants.remove(method);
                    }
                }
            }
        }
    }

    /// Does this plugin have authority to call `method` against
    /// `target_id` (or *any* target when `target_id` is None)?
    pub fn allows(&self, method: &str, target_id: Option<&str>) -> bool {
        let Some(scope) = self.grants.get(method) else {
            return false;
        };
        match target_id {
            None => scope.is_any(),
            Some(tid) => scope.covers(tid),
        }
    }

    /// Allowed subscribe filters at handshake time. The plugin host
    /// uses this to gate manifest `subscribes:` expansion across hot
    /// reloads (§9.4): the union of (initial install manifest +
    /// this list) is what's allowed for future handshakes.
    pub fn subscribes_allows(&self, filter: &str) -> bool {
        self.subscribes.allowed.iter().any(|s| s == filter)
    }

    /// Add a subscribe filter to the allowed-set. Deduplicates.
    pub fn add_subscribe(&mut self, filter: &str) {
        let set: BTreeSet<String> = self
            .subscribes
            .allowed
            .iter()
            .cloned()
            .chain(std::iter::once(filter.to_string()))
            .collect();
        self.subscribes.allowed = set.into_iter().collect();
    }
}

/// Reject `path` if it is itself a symlink. Closes the most likely
/// attack vector — an unprivileged process drops a symlink at the
/// grants.toml location pointing somewhere world-writable, so a
/// subsequent `save()` follows the link and writes to a third-party
/// file. We don't walk parent components because temp/system dirs
/// (e.g. `/var` → `/private/var` on macOS) routinely contain
/// symlinks we have no business policing.
fn reject_symlinks(path: &Path) -> Result<(), GrantsError> {
    if let Ok(meta) = std::fs::symlink_metadata(path)
        && meta.file_type().is_symlink()
    {
        return Err(GrantsError::Symlink(path.to_path_buf()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn roundtrip_blanket_and_targets() {
        let mut g = Grants::default();
        g.add("pane.snapshot", None);
        g.add("pane.send_keys", Some("a1b2"));
        g.add("pane.send_keys", Some("c3d4"));
        g.add("pane.send_keys", Some("a1b2")); // dedup
        g.add_subscribe("pane.exited");

        assert!(g.allows("pane.snapshot", None));
        assert!(g.allows("pane.snapshot", Some("any-target")));
        assert!(!g.allows("pane.send_keys", None));
        assert!(g.allows("pane.send_keys", Some("a1b2")));
        assert!(!g.allows("pane.send_keys", Some("e5f6")));

        let dir = tempdir().unwrap();
        let path = dir.path().join("grants.toml");
        g.save(&path).unwrap();
        let loaded = Grants::load(&path).unwrap();
        assert!(loaded.allows("pane.snapshot", Some("x")));
        assert!(loaded.allows("pane.send_keys", Some("a1b2")));
        assert!(!loaded.allows("pane.send_keys", Some("e5f6")));
        assert!(loaded.subscribes_allows("pane.exited"));
    }

    #[test]
    fn remove_blanket_then_target() {
        let mut g = Grants::default();
        g.add("pane.snapshot", None);
        g.remove("pane.snapshot", None);
        assert!(!g.allows("pane.snapshot", None));

        g.add("pane.send_keys", Some("a1"));
        g.add("pane.send_keys", Some("a2"));
        g.remove("pane.send_keys", Some("a1"));
        assert!(g.allows("pane.send_keys", Some("a2")));
        g.remove("pane.send_keys", Some("a2"));
        assert!(!g.grants.contains_key("pane.send_keys"));
    }

    #[test]
    fn load_missing_returns_empty() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nope.toml");
        let g = Grants::load(&path).unwrap();
        assert!(g.grants.is_empty());
    }

    #[test]
    fn reject_symlinked_grants_file() {
        let dir = tempdir().unwrap();
        let real = dir.path().join("real.toml");
        std::fs::write(&real, "[grants]\n").unwrap();
        let link = dir.path().join("link.toml");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real, &link).unwrap();
        #[cfg(not(unix))]
        return;

        let err = Grants::load(&link).unwrap_err();
        assert!(matches!(err, GrantsError::Symlink(_)));
    }
}
