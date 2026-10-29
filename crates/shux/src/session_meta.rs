//! Non-persisted, daemon-side decorations for sessions.
//!
//! Things the status bar wants to show that don't belong on the
//! `Session` model (no need for hot-reload, no need to survive daemon
//! restart, no programmatic API consumers): the git branch of the
//! session's spawn directory, an SSH-context flag, etc.
//!
//! Single source of truth for these signals so the renderer doesn't
//! re-shell-out on every frame.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use shux_core::model::SessionId;
use tokio::sync::RwLock;

/// One session's worth of decoration. All fields are best-effort —
/// failure to detect is silent.
#[derive(Debug, Clone, Default)]
pub struct SessionMeta {
    /// Result of `git symbolic-ref --short HEAD` run from the session's
    /// spawn cwd. `None` if not a git repo, git is missing, or detection
    /// timed out.
    pub git_branch: Option<String>,
    /// Whether this daemon process was reached over an SSH connection.
    /// Detected once at daemon-start from `$SSH_CONNECTION` / `$SSH_TTY`.
    pub over_ssh: bool,
}

/// Daemon-side cache. Cheap to clone — wraps an `Arc<RwLock<HashMap>>`.
#[derive(Clone, Default)]
pub struct SessionMetaCache {
    inner: Arc<RwLock<HashMap<SessionId, SessionMeta>>>,
}

impl SessionMetaCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn get(&self, sid: SessionId) -> SessionMeta {
        self.inner
            .read()
            .await
            .get(&sid)
            .cloned()
            .unwrap_or_default()
    }

    pub async fn set(&self, sid: SessionId, meta: SessionMeta) {
        self.inner.write().await.insert(sid, meta);
    }

    pub async fn remove(&self, sid: SessionId) {
        self.inner.write().await.remove(&sid);
    }
}

/// Detect the current git branch for `cwd`. Returns `None` for any
/// failure: cwd not a repo, git binary missing, command non-zero,
/// command times out, output not utf8. Best-effort, never panics.
///
/// Uses `git symbolic-ref --short HEAD` because it's the cheapest
/// branch query (no working-tree scan, just reads HEAD). Falls back to
/// the short hash via `git rev-parse --short HEAD` when HEAD is
/// detached.
pub fn detect_git_branch(cwd: &Path) -> Option<String> {
    // Generous for a HEAD-only query but bounded enough that a hung NFS /
    // fsmonitor won't stall daemon startup.
    detect_git_branch_within(cwd, std::time::Duration::from_millis(500))
}

/// [`detect_git_branch`], giving each git query up to `budget`.
fn detect_git_branch_within(cwd: &Path, budget: std::time::Duration) -> Option<String> {
    use std::process::{Command, Stdio};
    use std::time::Duration;

    let run = |args: &[&str]| -> Option<String> {
        let mut child = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .stdin(Stdio::null())
            .spawn()
            .ok()?;

        let deadline = std::time::Instant::now() + budget;
        loop {
            match child.try_wait() {
                Ok(Some(status)) if status.success() => {
                    use std::io::Read;
                    let mut buf = String::new();
                    child.stdout?.read_to_string(&mut buf).ok()?;
                    let trimmed = buf.trim().to_string();
                    return if trimmed.is_empty() {
                        None
                    } else {
                        Some(trimmed)
                    };
                }
                Ok(Some(_)) => return None,
                Ok(None) => {
                    if std::time::Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        return None;
                    }
                    #[expect(
                        clippy::disallowed_methods,
                        reason = "polls a git child that has no async handle, up to its budget"
                    )]
                    std::thread::sleep(Duration::from_millis(15));
                }
                Err(_) => return None,
            }
        }
    };

    run(&["symbolic-ref", "--short", "HEAD"]).or_else(|| {
        // Detached HEAD: fall back to short SHA.
        run(&["rev-parse", "--short", "HEAD"])
    })
}

/// True if this process appears to be running over an SSH connection.
/// Checked once at daemon startup; SessionMetaCache caches the boolean
/// onto each session.
pub fn detect_over_ssh() -> bool {
    std::env::var_os("SSH_CONNECTION").is_some() || std::env::var_os("SSH_TTY").is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_git_branch_in_this_repo() {
        // We're inside indrasvat/shux which is a git repo, so this
        // should yield Some(branch).
        // The test's budget, not the daemon's: git under a loaded test run
        // can take longer than the daemon is willing to wait.
        let cwd = std::env::current_dir().expect("cwd");
        let branch = detect_git_branch_within(&cwd, shux_testkit::BUDGET);
        assert!(
            branch.is_some(),
            "expected to detect a branch in the shux repo, got None"
        );
    }

    #[test]
    fn detect_git_branch_outside_repo() {
        // /tmp is reliably not a git repo on macOS and Linux.
        let branch = detect_git_branch(Path::new("/tmp"));
        // Note: this can FALSE-POSITIVE if /tmp is somehow inside a
        // worktree on a developer machine; the assert is "never panics
        // and returns a plausible result", not "always None".
        match branch {
            None => {}
            Some(b) => {
                // If somebody's /tmp is in a worktree, that's their problem,
                // not the function's. Just confirm we got a non-empty string.
                assert!(!b.is_empty());
            }
        }
    }

    #[tokio::test]
    async fn cache_round_trip() {
        let cache = SessionMetaCache::new();
        let sid = shux_core::model::SessionId::new();
        assert_eq!(cache.get(sid).await.git_branch, None);
        cache
            .set(
                sid,
                SessionMeta {
                    git_branch: Some("main".into()),
                    over_ssh: false,
                },
            )
            .await;
        assert_eq!(cache.get(sid).await.git_branch, Some("main".to_string()));
        cache.remove(sid).await;
        assert_eq!(cache.get(sid).await.git_branch, None);
    }
}
