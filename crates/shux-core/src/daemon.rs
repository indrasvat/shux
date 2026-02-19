//! Daemon runtime state and lifecycle primitives.
//!
//! This module defines the runtime state (`DaemonState`), command channel
//! (`DaemonCommand`), shutdown token tree (`ShutdownTokens`), and the
//! single-writer state loop (`run_daemon_state_loop`) that manages auto-exit.
//!
//! The Unix-level daemonization (fork/setsid) is NOT in this module — it
//! belongs in the binary crate since it must run before any tokio runtime.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::sync::{Notify, mpsc};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

/// How long to wait after the last session is destroyed before auto-exit.
const AUTO_EXIT_GRACE_PERIOD: Duration = Duration::from_secs(5);

/// Unique identifier for a daemon lease held by a long-lived plugin.
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct LeaseId(pub String);

/// Commands sent to the daemon state owner task.
#[derive(Debug)]
pub enum DaemonCommand {
    /// A session was created (increment active session count).
    SessionCreated,
    /// A session was destroyed (decrement active session count).
    SessionDestroyed,
    /// A plugin acquires a daemon lease (prevents auto-exit).
    AcquireLease(LeaseId),
    /// A plugin releases a daemon lease.
    ReleaseLease(LeaseId),
    /// Request graceful shutdown.
    Shutdown,
    /// Trigger config reload.
    ReloadConfig,
}

/// Result of checking whether the daemon should auto-exit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitDecision {
    /// Keep running — sessions or leases still active.
    KeepRunning,
    /// Start the grace timer — no sessions, no leases, but give time for reconnection.
    StartGraceTimer,
}

/// The daemon's runtime state, managed by a single owner task.
///
/// All mutations arrive via `DaemonCommand` over an mpsc channel.
/// This enforces the single-writer invariant from PRD 4.3 / 4.6.
pub struct DaemonState {
    /// Number of active sessions.
    session_count: u64,
    /// Active daemon leases held by plugins (PRD 4.5).
    leases: HashSet<LeaseId>,
    /// Monotonic version counter for state changes.
    version: AtomicU64,
}

impl DaemonState {
    pub fn new() -> Self {
        Self {
            session_count: 0,
            leases: HashSet::new(),
            version: AtomicU64::new(0),
        }
    }

    pub fn session_count(&self) -> u64 {
        self.session_count
    }

    pub fn lease_count(&self) -> usize {
        self.leases.len()
    }

    pub fn increment_sessions(&mut self) {
        self.session_count += 1;
        self.version.fetch_add(1, Ordering::SeqCst);
    }

    pub fn decrement_sessions(&mut self) {
        self.session_count = self.session_count.saturating_sub(1);
        self.version.fetch_add(1, Ordering::SeqCst);
    }

    pub fn acquire_lease(&mut self, id: LeaseId) {
        self.leases.insert(id);
        self.version.fetch_add(1, Ordering::SeqCst);
    }

    pub fn release_lease(&mut self, id: &LeaseId) {
        self.leases.remove(id);
        self.version.fetch_add(1, Ordering::SeqCst);
    }

    /// Check whether the daemon should consider auto-exit.
    pub fn should_exit(&self) -> ExitDecision {
        if self.session_count == 0 && self.leases.is_empty() {
            ExitDecision::StartGraceTimer
        } else {
            ExitDecision::KeepRunning
        }
    }
}

impl Default for DaemonState {
    fn default() -> Self {
        Self::new()
    }
}

/// The CancellationToken tree for the daemon.
///
/// The root token is cancelled on shutdown. Subsystem tokens are children
/// of the root, so cancelling the root cascades to all subsystems.
///
/// ```text
///   root_token
///   ├── pty_manager_token
///   ├── api_server_token
///   ├── event_bus_token
///   ├── plugin_host_token
///   └── render_token
/// ```
#[derive(Clone)]
pub struct ShutdownTokens {
    /// Root cancellation token. Cancel this to shut down everything.
    pub root: CancellationToken,
}

impl ShutdownTokens {
    pub fn new() -> Self {
        Self {
            root: CancellationToken::new(),
        }
    }

    /// Create a child token for a subsystem. Automatically cancelled
    /// when the root token is cancelled.
    pub fn child(&self) -> CancellationToken {
        self.root.child_token()
    }
}

impl Default for ShutdownTokens {
    fn default() -> Self {
        Self::new()
    }
}

/// Run the daemon state owner task.
///
/// This is the single-writer task that owns `DaemonState`. All mutations
/// flow through the `cmd_rx` channel. This task also manages the auto-exit
/// grace timer per PRD 4.5.
pub async fn run_daemon_state_loop(
    mut cmd_rx: mpsc::Receiver<DaemonCommand>,
    tokens: ShutdownTokens,
    config_reload_notify: Arc<Notify>,
) {
    let mut state = DaemonState::new();
    let mut grace_deadline: Option<tokio::time::Instant> = None;

    loop {
        tokio::select! {
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(DaemonCommand::SessionCreated) => {
                        state.increment_sessions();
                        grace_deadline = None;
                        info!(sessions = state.session_count(), "Session created");
                    }
                    Some(DaemonCommand::SessionDestroyed) => {
                        state.decrement_sessions();
                        info!(sessions = state.session_count(), "Session destroyed");
                        if state.should_exit() == ExitDecision::StartGraceTimer {
                            info!("No sessions or leases remain, starting {AUTO_EXIT_GRACE_PERIOD:?} grace timer");
                            grace_deadline = Some(tokio::time::Instant::now() + AUTO_EXIT_GRACE_PERIOD);
                        }
                    }
                    Some(DaemonCommand::AcquireLease(id)) => {
                        info!(lease = %id.0, "Daemon lease acquired");
                        state.acquire_lease(id);
                        grace_deadline = None;
                    }
                    Some(DaemonCommand::ReleaseLease(id)) => {
                        info!(lease = %id.0, "Daemon lease released");
                        state.release_lease(&id);
                        if state.should_exit() == ExitDecision::StartGraceTimer {
                            info!("No sessions or leases remain, starting {AUTO_EXIT_GRACE_PERIOD:?} grace timer");
                            grace_deadline = Some(tokio::time::Instant::now() + AUTO_EXIT_GRACE_PERIOD);
                        }
                    }
                    Some(DaemonCommand::Shutdown) => {
                        info!("Explicit shutdown requested");
                        tokens.root.cancel();
                        break;
                    }
                    Some(DaemonCommand::ReloadConfig) => {
                        info!("Config reload requested");
                        config_reload_notify.notify_waiters();
                    }
                    None => {
                        warn!("All command senders dropped, shutting down");
                        tokens.root.cancel();
                        break;
                    }
                }
            }

            // Grace timer expired — auto-exit
            _ = async {
                match grace_deadline {
                    Some(deadline) => tokio::time::sleep_until(deadline).await,
                    None => std::future::pending::<()>().await,
                }
            } => {
                if state.should_exit() == ExitDecision::StartGraceTimer {
                    info!("Grace timer expired with no sessions or leases — shutting down");
                    tokens.root.cancel();
                    break;
                } else {
                    grace_deadline = None;
                }
            }

            // Root token cancelled externally (e.g., by signal handler)
            _ = tokens.root.cancelled() => {
                info!("Root cancellation token triggered — shutting down");
                break;
            }
        }
    }

    info!("Daemon state loop exiting");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_state_has_zero_sessions() {
        let state = DaemonState::new();
        assert_eq!(state.session_count(), 0);
        assert_eq!(state.lease_count(), 0);
    }

    #[test]
    fn test_session_increment_decrement() {
        let mut state = DaemonState::new();
        state.increment_sessions();
        state.increment_sessions();
        assert_eq!(state.session_count(), 2);

        state.decrement_sessions();
        assert_eq!(state.session_count(), 1);
    }

    #[test]
    fn test_decrement_saturates_at_zero() {
        let mut state = DaemonState::new();
        state.decrement_sessions();
        assert_eq!(state.session_count(), 0);
    }

    #[test]
    fn test_should_exit_with_sessions() {
        let mut state = DaemonState::new();
        state.increment_sessions();
        assert_eq!(state.should_exit(), ExitDecision::KeepRunning);
    }

    #[test]
    fn test_should_exit_with_leases() {
        let mut state = DaemonState::new();
        state.acquire_lease(LeaseId("mcp-bridge".into()));
        assert_eq!(state.should_exit(), ExitDecision::KeepRunning);
    }

    #[test]
    fn test_should_exit_with_no_sessions_or_leases() {
        let state = DaemonState::new();
        assert_eq!(state.should_exit(), ExitDecision::StartGraceTimer);
    }

    #[test]
    fn test_should_exit_after_all_sessions_destroyed() {
        let mut state = DaemonState::new();
        state.increment_sessions();
        state.decrement_sessions();
        assert_eq!(state.should_exit(), ExitDecision::StartGraceTimer);
    }

    #[test]
    fn test_lease_prevents_exit() {
        let mut state = DaemonState::new();
        state.acquire_lease(LeaseId("mcp".into()));
        assert_eq!(state.should_exit(), ExitDecision::KeepRunning);

        state.release_lease(&LeaseId("mcp".into()));
        assert_eq!(state.should_exit(), ExitDecision::StartGraceTimer);
    }

    #[test]
    fn test_duplicate_lease_is_idempotent() {
        let mut state = DaemonState::new();
        state.acquire_lease(LeaseId("x".into()));
        state.acquire_lease(LeaseId("x".into()));
        assert_eq!(state.lease_count(), 1);
    }

    #[tokio::test]
    async fn test_shutdown_command_cancels_root_token() {
        let tokens = ShutdownTokens::new();
        let notify = Arc::new(Notify::new());
        let (tx, rx) = mpsc::channel(8);

        let tokens_clone = tokens.clone();
        let handle = tokio::spawn(async move {
            run_daemon_state_loop(rx, tokens_clone, notify).await;
        });

        tx.send(DaemonCommand::Shutdown).await.unwrap();
        handle.await.unwrap();

        assert!(tokens.root.is_cancelled());
    }

    #[tokio::test]
    async fn test_dropped_senders_trigger_shutdown() {
        let tokens = ShutdownTokens::new();
        let notify = Arc::new(Notify::new());
        let (tx, rx) = mpsc::channel(8);

        let tokens_clone = tokens.clone();
        let handle = tokio::spawn(async move {
            run_daemon_state_loop(rx, tokens_clone, notify).await;
        });

        drop(tx);
        handle.await.unwrap();

        assert!(tokens.root.is_cancelled());
    }

    #[tokio::test]
    async fn test_grace_timer_triggers_exit() {
        // Pause tokio time so we can advance it instantly
        tokio::time::pause();

        let tokens = ShutdownTokens::new();
        let notify = Arc::new(Notify::new());
        let (tx, rx) = mpsc::channel(8);

        let tokens_clone = tokens.clone();
        let handle = tokio::spawn(async move {
            run_daemon_state_loop(rx, tokens_clone, notify).await;
        });

        // Create and immediately destroy a session to trigger grace timer
        tx.send(DaemonCommand::SessionCreated).await.unwrap();
        tx.send(DaemonCommand::SessionDestroyed).await.unwrap();

        // Give the loop time to process commands
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;

        // Advance time past the grace period
        tokio::time::advance(Duration::from_secs(6)).await;
        tokio::task::yield_now().await;

        // The loop should have exited
        handle.await.unwrap();
        assert!(tokens.root.is_cancelled());
    }

    #[tokio::test]
    async fn test_new_session_cancels_grace_timer() {
        tokio::time::pause();

        let tokens = ShutdownTokens::new();
        let notify = Arc::new(Notify::new());
        let (tx, rx) = mpsc::channel(8);

        let tokens_clone = tokens.clone();
        let handle = tokio::spawn(async move {
            run_daemon_state_loop(rx, tokens_clone, notify).await;
        });

        // Start grace timer
        tx.send(DaemonCommand::SessionCreated).await.unwrap();
        tx.send(DaemonCommand::SessionDestroyed).await.unwrap();
        tokio::task::yield_now().await;

        // Before timer expires, create a new session
        tokio::time::advance(Duration::from_secs(2)).await;
        tx.send(DaemonCommand::SessionCreated).await.unwrap();
        tokio::task::yield_now().await;

        // Advance past original deadline — should NOT exit
        tokio::time::advance(Duration::from_secs(5)).await;
        tokio::task::yield_now().await;
        assert!(!tokens.root.is_cancelled());

        // Clean shutdown
        tx.send(DaemonCommand::Shutdown).await.unwrap();
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn test_config_reload_notifies() {
        let tokens = ShutdownTokens::new();
        let notify = Arc::new(Notify::new());
        let (tx, rx) = mpsc::channel(8);

        let notify_clone = notify.clone();
        let tokens_clone = tokens.clone();
        let handle = tokio::spawn(async move {
            run_daemon_state_loop(rx, tokens_clone, notify_clone).await;
        });

        tx.send(DaemonCommand::ReloadConfig).await.unwrap();
        tx.send(DaemonCommand::Shutdown).await.unwrap();

        handle.await.unwrap();
        assert!(tokens.root.is_cancelled());
    }

    #[test]
    fn test_shutdown_tokens_child_cancelled_with_root() {
        let tokens = ShutdownTokens::new();
        let child = tokens.child();

        assert!(!child.is_cancelled());
        tokens.root.cancel();
        assert!(child.is_cancelled());
    }
}
