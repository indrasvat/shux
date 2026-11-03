//! Request-scoped caller identity (lens PRD LENS-R-052 `caller` field;
//! P5 convergence round 1, claude N3 — adjudicated: task-local threading).
//!
//! shux's `Handler` trait deliberately receives only `params`; threading a
//! caller identity through ~60 handler signatures for one audit field would
//! be pure churn. Instead, the identity rides a `tokio::task_local!`:
//!
//! - The PLUGIN dispatch path (shux-plugin's `dispatch_plugin_frame`)
//!   already spawns a dedicated task per router dispatch; it wraps that
//!   dispatch future in [`RPC_CALLER`]`.scope("plugin:<uuid>", …)`.
//! - The UDS server path sets no scope, so [`current_caller`] falls back to
//!   `"uds"` — the correct default for the socket-owning local user (and
//!   for daemon-internal work like reap timers, which run in their own
//!   spawned tasks where no scope propagates).
//!
//! Scopes do NOT propagate across `tokio::spawn` — a task spawned from a
//! plugin-scoped request (e.g. a scratch reaper) intentionally reverts to
//! the daemon default.

use std::future::Future;

tokio::task_local! {
    /// The identity of the caller whose request the current task is
    /// serving: `"uds"` (implicit default — never set explicitly) or
    /// `"plugin:<uuid>"` (set by the plugin dispatch wrapper).
    pub static RPC_CALLER: String;
}

/// The current request's caller identity, defaulting to `"uds"` outside any
/// [`RPC_CALLER`] scope (UDS requests, daemon-internal tasks, tests).
pub fn current_caller() -> String {
    RPC_CALLER
        .try_with(|c| c.clone())
        .unwrap_or_else(|_| "uds".to_string())
}

/// Run `fut` with [`RPC_CALLER`] scoped to `caller`. Thin wrapper over
/// `RPC_CALLER.scope` so callers outside this crate don't need to know the
/// task-local's shape.
pub async fn with_caller<F: Future>(caller: String, fut: F) -> F::Output {
    RPC_CALLER.scope(caller, fut).await
}

/// Run a handler body to completion even if its caller hangs up. The server
/// drops a handler whose client disconnects, which is right for a wait and
/// wrong for a change made in several steps: cut between them, it leaves the
/// change half done. The body keeps the caller's identity; it runs outside
/// the request's connection scope.
pub async fn run_to_completion<F>(fut: F) -> Result<serde_json::Value, crate::RpcError>
where
    F: Future<Output = Result<serde_json::Value, crate::RpcError>> + Send + 'static,
{
    tokio::spawn(with_caller(current_caller(), fut))
        .await
        .map_err(|e| crate::RpcError::internal(&format!("handler task failed: {e}")))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn defaults_to_uds_outside_any_scope() {
        assert_eq!(current_caller(), "uds");
    }

    #[tokio::test]
    async fn scope_sets_and_restores_the_identity() {
        let inside = with_caller("plugin:abc-123".to_string(), async { current_caller() }).await;
        assert_eq!(inside, "plugin:abc-123");
        // Back outside the scope: default again.
        assert_eq!(current_caller(), "uds");
    }

    #[tokio::test]
    async fn scope_does_not_propagate_across_spawn() {
        let spawned = with_caller("plugin:abc-123".to_string(), async {
            tokio::spawn(async { current_caller() }).await.unwrap()
        })
        .await;
        assert_eq!(
            spawned, "uds",
            "tokio::spawn starts a fresh task without the scope"
        );
    }

    #[tokio::test]
    async fn a_body_run_to_completion_keeps_its_caller() {
        let seen = with_caller(
            "plugin:abc-123".to_string(),
            run_to_completion(async { Ok(serde_json::json!(current_caller())) }),
        )
        .await
        .expect("body");
        assert_eq!(
            seen, "plugin:abc-123",
            "the audit would name the wrong caller"
        );
    }
}
