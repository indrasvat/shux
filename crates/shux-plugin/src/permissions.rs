//! Pure decision function — given a plugin's identity, the method
//! being called, the resolved targets + their owners, and the
//! plugin's grants, decide allow or deny.
//!
//! No I/O. Caller (`dispatch_plugin_frame`) is responsible for
//! loading grants, looking up entity ownership from the graph
//! snapshot, and recording the decision in the audit log.
//!
//! See `docs/designs/permissions/README.md` §§5, 9 for the design.

use serde_json::Value;
use shux_core::model::PluginId;
use shux_rpc::policy::{Policy, Sensitivity};

use crate::grants::Grants;

/// Targets resolved from a call's params. At most one of each kind;
/// methods that act on multiple entities flatten into separate
/// permission checks at the call site.
#[derive(Debug, Default, Clone)]
pub struct Targets {
    pub session_id: Option<String>,
    pub window_id: Option<String>,
    pub pane_id: Option<String>,
}

impl Targets {
    pub fn is_empty(&self) -> bool {
        self.session_id.is_none() && self.window_id.is_none() && self.pane_id.is_none()
    }

    /// Pick the "primary" target id used for grant matching. Pane
    /// takes precedence over window over session, on the principle
    /// that more specific = better scoped.
    pub fn primary(&self) -> Option<&str> {
        self.pane_id
            .as_deref()
            .or(self.window_id.as_deref())
            .or(self.session_id.as_deref())
    }
}

/// Resolved owner of a target. `None` means user-created (default).
/// The caller resolves this from `Pane.created_by_plugin` etc.
#[derive(Debug, Default, Clone)]
pub struct TargetOwners {
    pub pane_owner: Option<PluginId>,
    pub window_owner: Option<PluginId>,
    pub session_owner: Option<PluginId>,
}

impl TargetOwners {
    /// Are all named targets in `targets` owned by `plugin_id`?
    /// Missing targets (None in `targets`) are vacuously true.
    pub fn all_owned_by(&self, plugin_id: &PluginId, targets: &Targets) -> bool {
        let check = |target_present: bool, owner: Option<&PluginId>| -> bool {
            if target_present {
                owner == Some(plugin_id)
            } else {
                true
            }
        };
        check(targets.pane_id.is_some(), self.pane_owner.as_ref())
            && check(targets.window_id.is_some(), self.window_owner.as_ref())
            && check(targets.session_id.is_some(), self.session_owner.as_ref())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allow { reason: AllowReason },
    Deny { reason: DenyReason },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AllowReason {
    PublicMethod,
    PluginSelfNamespace,
    OwnedByPlugin,
    ExplicitGrant,
    UnclassifiedRoute,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DenyReason {
    PluginsForbidden,
    NoGrantAndNotOwned,
    NoGrant,
}

impl Decision {
    pub fn is_allow(&self) -> bool {
        matches!(self, Decision::Allow { .. })
    }

    pub fn label(&self) -> &'static str {
        if self.is_allow() { "allow" } else { "deny" }
    }

    pub fn reason_str(&self) -> &'static str {
        match self {
            Decision::Allow { reason } => match reason {
                AllowReason::PublicMethod => "public_method",
                AllowReason::PluginSelfNamespace => "plugin_self_namespace",
                AllowReason::OwnedByPlugin => "owned_by_plugin",
                AllowReason::ExplicitGrant => "explicit_grant",
                AllowReason::UnclassifiedRoute => "unclassified_route",
            },
            Decision::Deny { reason } => match reason {
                DenyReason::PluginsForbidden => "plugins_forbidden",
                DenyReason::NoGrantAndNotOwned => "no_grant_and_not_owned",
                DenyReason::NoGrant => "no_grant",
            },
        }
    }
}

/// Bundle of inputs to [`check`]. Grouped to satisfy clippy's
/// `too_many_arguments` lint and to make call sites self-documenting.
pub struct CheckCtx<'a> {
    pub plugin_id: &'a PluginId,
    /// Display name — used only by the `events.watch` self-scope
    /// classifier (`plugin.<name>.*` filters).
    pub plugin_name: &'a str,
    pub method: &'a str,
    /// Classification from `Router::policy(method)`. `None` means the
    /// route has no declared policy — the startup assertion should
    /// have caught this; we soft-fail with `UnclassifiedRoute`
    /// rather than crash mid-request.
    pub policy: Option<&'a Policy>,
    pub params: Option<&'a Value>,
    pub targets: &'a Targets,
    pub owners: &'a TargetOwners,
    pub grants: &'a Grants,
}

/// The core decision. Pure function — caller wires in I/O.
pub fn check(ctx: &CheckCtx<'_>) -> Decision {
    let Some(policy) = ctx.policy else {
        // Soft-fail open with a flagged reason. Startup assertion
        // is the real defence; this branch only fires if a route
        // dodges classification AND boot validation is disabled
        // (e.g. test harnesses building partial routers).
        return Decision::Allow {
            reason: AllowReason::UnclassifiedRoute,
        };
    };
    let sensitivity = policy.resolve(ctx.params, ctx.plugin_name);
    let plugin_id = ctx.plugin_id;
    let method = ctx.method;
    let targets = ctx.targets;
    let owners = ctx.owners;
    let grants = ctx.grants;

    match sensitivity {
        Sensitivity::Public => Decision::Allow {
            reason: AllowReason::PublicMethod,
        },
        Sensitivity::PluginsForbidden => Decision::Deny {
            reason: DenyReason::PluginsForbidden,
        },
        Sensitivity::ContentRead | Sensitivity::OwnedMutation => {
            // Ownership auto-grant when the plugin created every
            // named target. Empty `targets` falls through to grant
            // check — methods with no target are still gated.
            if !targets.is_empty() && owners.all_owned_by(plugin_id, targets) {
                return Decision::Allow {
                    reason: AllowReason::OwnedByPlugin,
                };
            }
            if grants.allows(method, targets.primary()) {
                Decision::Allow {
                    reason: AllowReason::ExplicitGrant,
                }
            } else {
                Decision::Deny {
                    reason: DenyReason::NoGrantAndNotOwned,
                }
            }
        }
        Sensitivity::Grantable => {
            // No ownership shortcut: explicit grant only. `*` scope
            // is the only path; per-target grants don't make sense
            // for whole-graph batch ops like `state.apply`.
            if grants.allows(method, None) {
                Decision::Allow {
                    reason: AllowReason::ExplicitGrant,
                }
            } else {
                Decision::Deny {
                    reason: DenyReason::NoGrant,
                }
            }
        }
    }
}

/// Extract `session_id` / `window_id` / `pane_id` from a call's
/// params object. Returns an empty `Targets` for params with no
/// recognised ID fields.
pub fn extract_targets(params: Option<&Value>) -> Targets {
    let Some(obj) = params.and_then(|v| v.as_object()) else {
        return Targets::default();
    };
    let pull =
        |key: &str| -> Option<String> { obj.get(key).and_then(|v| v.as_str()).map(String::from) };
    Targets {
        session_id: pull("session_id"),
        window_id: pull("window_id"),
        pane_id: pull("pane_id"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shux_rpc::Policy as RpcPolicy;
    use shux_rpc::Sensitivity as RpcSens;

    fn pid() -> PluginId {
        PluginId::new()
    }

    #[test]
    fn public_always_allows() {
        let policy = RpcPolicy::fixed(RpcSens::Public);
        let me = pid();
        let d = check(&CheckCtx {
            plugin_id: &me,
            plugin_name: "p",
            method: "session.list",
            policy: Some(&policy),
            params: None,
            targets: &Targets::default(),
            owners: &TargetOwners::default(),
            grants: &Grants::default(),
        });
        assert_eq!(d.label(), "allow");
        assert_eq!(d.reason_str(), "public_method");
    }

    #[test]
    fn plugins_forbidden_never_grantable() {
        let policy = RpcPolicy::fixed(RpcSens::PluginsForbidden);
        let mut g = Grants::default();
        g.add("plugin.install", None); // even with grant, denied
        let me = pid();
        let d = check(&CheckCtx {
            plugin_id: &me,
            plugin_name: "p",
            method: "plugin.install",
            policy: Some(&policy),
            params: None,
            targets: &Targets::default(),
            owners: &TargetOwners::default(),
            grants: &g,
        });
        assert!(!d.is_allow());
        assert_eq!(d.reason_str(), "plugins_forbidden");
    }

    #[test]
    fn owned_pane_send_keys_auto_allows() {
        let me = pid();
        let policy = RpcPolicy::fixed(RpcSens::OwnedMutation);
        let targets = Targets {
            pane_id: Some("abc".to_string()),
            ..Default::default()
        };
        let owners = TargetOwners {
            pane_owner: Some(me),
            ..Default::default()
        };
        let d = check(&CheckCtx {
            plugin_id: &me,
            plugin_name: "p",
            method: "pane.send_keys",
            policy: Some(&policy),
            params: None,
            targets: &targets,
            owners: &owners,
            grants: &Grants::default(),
        });
        assert!(d.is_allow());
        assert_eq!(d.reason_str(), "owned_by_plugin");
    }

    #[test]
    fn unowned_pane_send_keys_needs_grant() {
        let me = pid();
        let other = pid();
        let policy = RpcPolicy::fixed(RpcSens::OwnedMutation);
        let targets = Targets {
            pane_id: Some("abc".to_string()),
            ..Default::default()
        };
        let owners = TargetOwners {
            pane_owner: Some(other),
            ..Default::default()
        };
        let mut grants = Grants::default();

        let d = check(&CheckCtx {
            plugin_id: &me,
            plugin_name: "p",
            method: "pane.send_keys",
            policy: Some(&policy),
            params: None,
            targets: &targets,
            owners: &owners,
            grants: &grants,
        });
        assert!(!d.is_allow());
        assert_eq!(d.reason_str(), "no_grant_and_not_owned");

        grants.add("pane.send_keys", Some("abc"));
        let d = check(&CheckCtx {
            plugin_id: &me,
            plugin_name: "p",
            method: "pane.send_keys",
            policy: Some(&policy),
            params: None,
            targets: &targets,
            owners: &owners,
            grants: &grants,
        });
        assert!(d.is_allow());
        assert_eq!(d.reason_str(), "explicit_grant");
    }

    #[test]
    fn state_apply_needs_blanket_grant() {
        let policy = RpcPolicy::fixed(RpcSens::Grantable);
        let mut grants = Grants::default();
        let me = pid();

        let mk_ctx = |grants_ref: &Grants| -> Decision {
            check(&CheckCtx {
                plugin_id: &me,
                plugin_name: "p",
                method: "state.apply",
                policy: Some(&policy),
                params: None,
                targets: &Targets::default(),
                owners: &TargetOwners::default(),
                grants: grants_ref,
            })
        };

        assert!(!mk_ctx(&grants).is_allow());

        // Target-scoped grant does NOT help: Grantable needs `*`.
        grants.add("state.apply", Some("some-id"));
        assert!(!mk_ctx(&grants).is_allow());

        grants.add("state.apply", None);
        assert!(mk_ctx(&grants).is_allow());
    }

    #[test]
    fn events_watch_self_namespace_is_public() {
        let policy = RpcPolicy::param_aware(|params, plugin_id| {
            let f = params
                .and_then(|p| p.get("filter"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if f.starts_with(&format!("plugin.{plugin_id}.")) {
                RpcSens::Public
            } else {
                RpcSens::ContentRead
            }
        });

        let me = pid();
        let mk_ctx = |params: Option<&Value>| -> Decision {
            check(&CheckCtx {
                plugin_id: &me,
                plugin_name: "watcher",
                method: "events.watch",
                policy: Some(&policy),
                params,
                targets: &Targets::default(),
                owners: &TargetOwners::default(),
                grants: &Grants::default(),
            })
        };

        let self_scoped = serde_json::json!({"filter": "plugin.watcher.command_exit"});
        let d = mk_ctx(Some(&self_scoped));
        assert!(d.is_allow());
        assert_eq!(d.reason_str(), "public_method");

        let firehose = serde_json::json!({"filter": "pane."});
        let d = mk_ctx(Some(&firehose));
        assert!(!d.is_allow());
    }

    #[test]
    fn extract_targets_pulls_id_fields() {
        let p = serde_json::json!({
            "pane_id": "p-1",
            "window_id": "w-1",
            "session_id": "s-1",
            "extra": "ignored",
        });
        let t = extract_targets(Some(&p));
        assert_eq!(t.pane_id.as_deref(), Some("p-1"));
        assert_eq!(t.window_id.as_deref(), Some("w-1"));
        assert_eq!(t.session_id.as_deref(), Some("s-1"));
        assert_eq!(t.primary(), Some("p-1"));
    }
}
