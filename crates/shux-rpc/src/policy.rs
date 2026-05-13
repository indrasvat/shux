//! Per-method sensitivity classification — consumed by the plugin
//! permission enforcer (`shux-plugin::permissions`).
//!
//! Direct CLI clients bypass these checks; they ARE the user. The
//! plugin dispatcher (`dispatch_plugin_frame`) consults `Router::policy`
//! before forwarding any plugin-originated RPC frame.
//!
//! See `docs/designs/permissions/README.md` §9.6 for the design.

use serde_json::Value;

/// Sensitivity tier of an RPC method. Determines the default
/// authority a plugin needs to call it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sensitivity {
    /// Tier 0 — informational, no exfil potential beyond entity IDs.
    /// Always allowed (e.g. `session.list`, `window.list`).
    Public,
    /// Tier 1 — reads VT / event content; exfil risk if target was
    /// not created by the calling plugin (e.g. `pane.capture`,
    /// `pane.snapshot`, `pane.output.watch`).
    ContentRead,
    /// Tier 2 — mutates entities; ownership-gated. Auto-allowed if
    /// the calling plugin created the target; needs explicit grant
    /// otherwise (e.g. `pane.send_keys`, `pane.kill`, `window.create`).
    OwnedMutation,
    /// Tier 3b — grantable but never default-allow (e.g. `state.apply`).
    /// Requires an explicit grant with no target-scope auto-fallback.
    Grantable,
    /// Tier 3a — flat-deny to all plugins. Plugins cannot manage
    /// other plugins. No grant path (e.g. `plugin.install`,
    /// `plugin.kill`, `plugin.reload`).
    PluginsForbidden,
}

/// Closure form of a parameter-aware policy. Boxed/Arc'd in `Policy::ParamAware`.
pub type PolicyFn = dyn Fn(Option<&Value>, &str) -> Sensitivity + Send + Sync;

/// Per-method policy. Most methods are a single `Sensitivity`; a few
/// (`events.watch`) are parameter-dependent and use the closure form.
#[derive(Clone)]
pub enum Policy {
    /// Fixed sensitivity regardless of params.
    Fixed(Sensitivity),
    /// Parameter-aware classifier. The closure receives the call's
    /// `params` and the calling plugin's id and returns a sensitivity.
    /// Used for `events.watch` where `filter == "plugin.<self>."` is
    /// `Public` but a broader filter is `ContentRead`.
    ParamAware(std::sync::Arc<PolicyFn>),
}

impl Policy {
    pub fn fixed(s: Sensitivity) -> Self {
        Policy::Fixed(s)
    }

    pub fn param_aware<F>(f: F) -> Self
    where
        F: Fn(Option<&Value>, &str) -> Sensitivity + Send + Sync + 'static,
    {
        Policy::ParamAware(std::sync::Arc::new(f))
    }

    /// Resolve this policy to a concrete sensitivity for a specific
    /// call. `plugin_id` is the calling plugin's display name (already
    /// authenticated by the I/O loop — plugins cannot spoof it).
    pub fn resolve(&self, params: Option<&Value>, plugin_id: &str) -> Sensitivity {
        match self {
            Policy::Fixed(s) => *s,
            Policy::ParamAware(f) => f(params, plugin_id),
        }
    }
}

impl std::fmt::Debug for Policy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Policy::Fixed(s) => write!(f, "Policy::Fixed({s:?})"),
            Policy::ParamAware(_) => write!(f, "Policy::ParamAware(<closure>)"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_resolves_to_inner() {
        let p = Policy::fixed(Sensitivity::OwnedMutation);
        assert_eq!(p.resolve(None, "test-plugin"), Sensitivity::OwnedMutation);
    }

    #[test]
    fn events_watch_param_aware_self_scope_is_public() {
        let p = Policy::param_aware(|params, plugin_id| {
            let f = params
                .and_then(|p| p.get("filter"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if f.starts_with(&format!("plugin.{plugin_id}.")) {
                Sensitivity::Public
            } else {
                Sensitivity::ContentRead
            }
        });

        let self_scoped = serde_json::json!({"filter": "plugin.watcher.command_exit"});
        assert_eq!(
            p.resolve(Some(&self_scoped), "watcher"),
            Sensitivity::Public
        );

        let firehose = serde_json::json!({"filter": "pane."});
        assert_eq!(
            p.resolve(Some(&firehose), "watcher"),
            Sensitivity::ContentRead
        );

        assert_eq!(p.resolve(None, "watcher"), Sensitivity::ContentRead);
    }
}
