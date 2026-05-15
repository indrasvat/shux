//! First-run onboarding state.
//!
//! Stored at `$XDG_STATE_HOME/shux/onboarding.json` (or
//! `$HOME/.local/state/shux/onboarding.json`). Single source of truth
//! for "should the OOTB hint still show?" and "should the first-attach
//! toast still show?". Loaded once at daemon start; written when the
//! user observably-discovers a piece of UX (tap the prefix → dismiss
//! the prefix hint; receive the toast → mark toast as seen).
//!
//! Schema is intentionally minimal — we want a one-bit-per-feature
//! decay model that's trivially serializable, forward-compatible (new
//! booleans default to false), and easy to reset (`rm
//! ~/.local/state/shux/onboarding.json` brings every hint back).

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

/// Defaults to "every hint is fresh, nothing dismissed".
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OnboardingState {
    /// True once the user has tapped the prefix key at least once
    /// inside any attach. Drives the right-zone hint dismissal.
    #[serde(default)]
    pub prefix_discovered: bool,
    /// True once the first-attach welcome toast has been shown end-to-end
    /// (~3s after first attach). Suppresses the toast on subsequent attaches.
    #[serde(default)]
    pub welcome_toast_seen: bool,
}

#[derive(Clone)]
pub struct OnboardingHandle {
    inner: Arc<RwLock<OnboardingState>>,
    path: PathBuf,
}

impl OnboardingHandle {
    /// Load from disk if present, else empty defaults. Never errors:
    /// missing file, parse failures, permission errors all collapse to
    /// the default state (the user sees every hint, which is the right
    /// failure mode).
    pub fn load() -> Self {
        let path = state_file_path();
        let state = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self {
            inner: Arc::new(RwLock::new(state)),
            path,
        }
    }

    pub async fn current(&self) -> OnboardingState {
        self.inner.read().await.clone()
    }

    /// Mark the prefix as discovered and persist. Subsequent attaches
    /// will see the hint dismissed.
    pub async fn mark_prefix_discovered(&self) {
        let mut g = self.inner.write().await;
        if g.prefix_discovered {
            return;
        }
        g.prefix_discovered = true;
        let snapshot = g.clone();
        drop(g);
        self.persist(snapshot);
    }

    /// Mark the welcome toast as seen so the next attach doesn't show it.
    pub async fn mark_welcome_toast_seen(&self) {
        let mut g = self.inner.write().await;
        if g.welcome_toast_seen {
            return;
        }
        g.welcome_toast_seen = true;
        let snapshot = g.clone();
        drop(g);
        self.persist(snapshot);
    }

    /// Write the current state. Best-effort: errors are logged but never
    /// surfaced — losing the dismissal "just" means a hint reappears on
    /// next attach, which is harmless.
    fn persist(&self, state: OnboardingState) {
        let path = self.path.clone();
        // Spawn-blocking the FS work; we hold the lock briefly above
        // and don't await on IO inside the lock.
        tokio::task::spawn_blocking(move || {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            match serde_json::to_string_pretty(&state) {
                Ok(s) => {
                    if let Err(e) = std::fs::write(&path, s) {
                        tracing::debug!(error = %e, path = %path.display(), "onboarding: write failed");
                    }
                }
                Err(e) => {
                    tracing::debug!(error = %e, "onboarding: serialize failed");
                }
            }
        });
    }
}

fn state_file_path() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_STATE_HOME") {
        return PathBuf::from(xdg).join("shux").join("onboarding.json");
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home)
            .join(".local")
            .join("state")
            .join("shux")
            .join("onboarding.json");
    }
    PathBuf::from("onboarding.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn round_trip_dismissal() {
        // Redirect XDG_STATE_HOME to a tempdir so we don't clobber the
        // dev's real onboarding state.
        let tmp = tempfile::tempdir().unwrap();
        // SAFETY: edition 2024 made set_var unsafe; this is a test
        // process exclusively under our control.
        unsafe {
            std::env::set_var("XDG_STATE_HOME", tmp.path());
        }

        let handle = OnboardingHandle::load();
        assert!(!handle.current().await.prefix_discovered);

        handle.mark_prefix_discovered().await;
        // Give the spawn_blocking writer a moment.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(handle.current().await.prefix_discovered);

        // Reload from disk → still dismissed.
        let handle2 = OnboardingHandle::load();
        assert!(handle2.current().await.prefix_discovered);

        unsafe {
            std::env::remove_var("XDG_STATE_HOME");
        }
    }
}
