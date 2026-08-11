//! The plugin management RPC surface.

use std::path::PathBuf;

use shux_rpc::{Policy, Sensitivity};

/// Map PluginError → RpcError so plugin RPC handlers report
/// human-readable failures (NotFound + NameConflict reuse the
/// canonical PRD §8.3 error envelopes, everything else is
/// internal).
fn plugin_error_to_rpc(e: shux_plugin::PluginError) -> shux_rpc::RpcError {
    use shux_plugin::PluginError;
    match e {
        PluginError::NotFound(ref name) => shux_rpc::RpcError::not_found("plugin", name),
        PluginError::NameConflict(ref name) => shux_rpc::RpcError::name_conflict("plugin", name),
        PluginError::HandshakeFailed(_) | PluginError::Proto(_) => {
            shux_rpc::RpcError::invalid_params(&e.to_string())
        }
        PluginError::Io(_) => shux_rpc::RpcError::internal(&e.to_string()),
    }
}

/// Plugin RPC surface (task 044a, phase 0).
///
/// - `plugin.install` — spawn a plugin from a `path` (+ optional
///   `args`, `cwd`). Performs the handshake synchronously and
///   returns the resolved `PluginInfo`.
/// - `plugin.list` — snapshot of every running plugin.
/// - `plugin.kill` — graceful shutdown + child cleanup.
pub(crate) fn register_plugin_methods(
    builder: shux_rpc::RouterBuilder,
    plugins: shux_plugin::PluginManager,
) -> shux_rpc::RouterBuilder {
    let p1 = plugins.clone();
    let p2 = plugins.clone();
    let p3 = plugins.clone();
    let p4 = plugins.clone();
    let p5 = plugins.clone();
    let p6 = plugins.clone();
    let p7 = plugins.clone();
    let p8 = plugins;

    builder
        .register_with_policy(
            "plugin.install",
            Policy::fixed(Sensitivity::PluginsForbidden),
            move |params: Option<serde_json::Value>| {
                let mgr = p1.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let path = params
                        .get("path")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| shux_rpc::RpcError::invalid_params("missing 'path'"))?;
                    let args: Vec<String> = params
                        .get("args")
                        .and_then(|v| v.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                                .collect()
                        })
                        .unwrap_or_default();
                    let cwd = params
                        .get("cwd")
                        .and_then(|v| v.as_str())
                        .map(PathBuf::from);
                    // Watch defaults to ON — the dogfood loop showed
                    // every iteration without hot reload felt long.
                    // Callers opt out with `"watch": false`.
                    let watch = params
                        .get("watch")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(true);
                    // Per-install state root (codex P2 review on PR #32):
                    // a daemon shared across project checkouts must
                    // pin each plugin's state to the calling client's
                    // project, not to the daemon's own cwd. The CLI
                    // passes the resolved `.shux/plugins` path here.
                    let state_root = params
                        .get("state_root")
                        .and_then(|v| v.as_str())
                        .map(PathBuf::from);
                    let expected_name = params
                        .get("expected_name")
                        .and_then(|v| v.as_str())
                        .map(str::to_string);
                    let expected_version = params
                        .get("expected_version")
                        .and_then(|v| v.as_str())
                        .map(str::to_string);

                    let source = shux_plugin::PluginSource {
                        path: PathBuf::from(path),
                        args,
                        cwd,
                        watch,
                        state_root,
                        expected_name,
                        expected_version,
                    };
                    let info = mgr.install(source).await.map_err(plugin_error_to_rpc)?;
                    serde_json::to_value(&info).map_err(|e| {
                        shux_rpc::RpcError::internal(&format!("plugin info serialize: {e}"))
                    })
                }
            },
        )
        .register_with_policy("plugin.list", Policy::fixed(Sensitivity::Public), move |_params: Option<serde_json::Value>| {
            let mgr = p2.clone();
            async move {
                let infos = mgr.list().await;
                Ok(serde_json::json!({ "plugins": infos }))
            }
        })
        .register_with_policy("plugin.kill", Policy::fixed(Sensitivity::PluginsForbidden), move |params: Option<serde_json::Value>| {
            let mgr = p3.clone();
            async move {
                let params = params.unwrap_or_default();
                let name = params
                    .get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| shux_rpc::RpcError::invalid_params("missing 'name'"))?
                    .to_string();
                mgr.kill(&name).await.map_err(plugin_error_to_rpc)?;
                Ok(serde_json::json!({ "killed": name }))
            }
        })
        .register_with_policy("plugin.reload", Policy::fixed(Sensitivity::PluginsForbidden), move |params: Option<serde_json::Value>| {
            let mgr = p4.clone();
            async move {
                let params = params.unwrap_or_default();
                let name = params
                    .get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| shux_rpc::RpcError::invalid_params("missing 'name'"))?
                    .to_string();
                let info = mgr.reload(&name).await.map_err(plugin_error_to_rpc)?;
                serde_json::to_value(&info).map_err(|e| {
                    shux_rpc::RpcError::internal(&format!("plugin info serialize: {e}"))
                })
            }
        })
        .register_with_policy("plugin.grant", Policy::fixed(Sensitivity::PluginsForbidden), move |params: Option<serde_json::Value>| {
            let mgr = p5.clone();
            async move {
                let p = params.unwrap_or_default();
                let plugin = p.get("plugin").and_then(|v| v.as_str())
                    .ok_or_else(|| shux_rpc::RpcError::invalid_params("missing 'plugin'"))?.to_string();
                let method = p.get("method").and_then(|v| v.as_str())
                    .ok_or_else(|| shux_rpc::RpcError::invalid_params("missing 'method'"))?.to_string();
                let target = p.get("target").and_then(|v| v.as_str()).map(String::from);
                let subscribe = p.get("subscribe").and_then(|v| v.as_bool()).unwrap_or(false);
                mgr.grant(&plugin, &method, target.as_deref(), subscribe).await.map_err(plugin_error_to_rpc)?;
                Ok(serde_json::json!({"granted": true, "plugin": plugin, "method": method, "target": target, "subscribe": subscribe}))
            }
        })
        .register_with_policy("plugin.revoke", Policy::fixed(Sensitivity::PluginsForbidden), move |params: Option<serde_json::Value>| {
            let mgr = p6.clone();
            async move {
                let p = params.unwrap_or_default();
                let plugin = p.get("plugin").and_then(|v| v.as_str())
                    .ok_or_else(|| shux_rpc::RpcError::invalid_params("missing 'plugin'"))?.to_string();
                let method = p.get("method").and_then(|v| v.as_str())
                    .ok_or_else(|| shux_rpc::RpcError::invalid_params("missing 'method'"))?.to_string();
                let target = p.get("target").and_then(|v| v.as_str()).map(String::from);
                let subscribe = p.get("subscribe").and_then(|v| v.as_bool()).unwrap_or(false);
                mgr.revoke(&plugin, &method, target.as_deref(), subscribe).await.map_err(plugin_error_to_rpc)?;
                Ok(serde_json::json!({"revoked": true, "plugin": plugin, "method": method, "target": target, "subscribe": subscribe}))
            }
        })
        .register_with_policy("plugin.grants", Policy::fixed(Sensitivity::PluginsForbidden), move |params: Option<serde_json::Value>| {
            let mgr = p7.clone();
            async move {
                let p = params.unwrap_or_default();
                let plugin = p.get("plugin").and_then(|v| v.as_str())
                    .ok_or_else(|| shux_rpc::RpcError::invalid_params("missing 'plugin'"))?.to_string();
                let grants = mgr.grants_for(&plugin).await.map_err(plugin_error_to_rpc)?;
                serde_json::to_value(&grants).map_err(|e| shux_rpc::RpcError::internal(&format!("grants serialize: {e}")))
            }
        })
        .register_with_policy("plugin.audit", Policy::fixed(Sensitivity::PluginsForbidden), move |params: Option<serde_json::Value>| {
            let mgr = p8.clone();
            async move {
                let p = params.unwrap_or_default();
                let plugin = p.get("plugin").and_then(|v| v.as_str())
                    .ok_or_else(|| shux_rpc::RpcError::invalid_params("missing 'plugin'"))?.to_string();
                let tail = p.get("tail").and_then(|v| v.as_u64()).unwrap_or(50) as usize;
                let path = mgr.audit_path(&plugin).await.map_err(plugin_error_to_rpc)?;
                let body = match std::fs::read_to_string(&path) {
                    Ok(s) => s,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
                    Err(e) => return Err(shux_rpc::RpcError::internal(&format!("read audit log {}: {e}", path.display()))),
                };
                let mut entries: Vec<serde_json::Value> = body
                    .lines()
                    .filter(|l| !l.is_empty())
                    .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
                    .collect();
                if tail > 0 && entries.len() > tail {
                    entries = entries.split_off(entries.len() - tail);
                }
                Ok(serde_json::json!({
                    "plugin": plugin,
                    "path": path.display().to_string(),
                    "entries": entries,
                }))
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::test_harness::{dispatch_err, dispatch_ok, write_plugin_script};

    #[tokio::test]
    async fn production_plugin_router_covers_install_grants_audit_reload_and_kill() {
        const NOOP_PLUGIN: &str = r#"#!/usr/bin/env bash
set -u
IFS= read -r _ || exit 1
printf '%s\n' '{"jsonrpc":"2.0","id":"init","result":{"name":"noop","version":"0.1.0","subscribes":[],"provides":[],"capabilities":[]}}'
while IFS= read -r line; do
  case "$line" in
    *'"plugin.shutdown"'*) exit 0 ;;
  esac
done
"#;

        let tmp = tempfile::tempdir().unwrap();
        let script = write_plugin_script(tmp.path(), "noop.sh", NOOP_PLUGIN);
        let manager = shux_plugin::PluginManager::with_state_root(
            shux_core::bus::EventBus::new(),
            tmp.path().join("plugins"),
        );
        let router = register_plugin_methods(shux_rpc::Router::builder(), manager.clone()).build();
        router.assert_every_route_has_policy();

        let missing_path = dispatch_err(&router, "plugin.install", serde_json::json!({})).await;
        assert_eq!(missing_path.code, shux_rpc::ErrorCode::InvalidParams.code());

        let identity_mismatch = dispatch_err(
            &router,
            "plugin.install",
            serde_json::json!({
                "path": script.clone(),
                "watch": false,
                "expected_name": "not-noop",
                "expected_version": "0.1.0",
            }),
        )
        .await;
        assert_eq!(
            identity_mismatch.code,
            shux_rpc::ErrorCode::InvalidParams.code()
        );
        assert!(
            identity_mismatch
                .data
                .as_ref()
                .and_then(|data| data.get("detail"))
                .and_then(|detail| detail.as_str())
                .unwrap_or_default()
                .contains("plugin manifest name mismatch")
        );

        let installed = dispatch_ok(
            &router,
            "plugin.install",
            serde_json::json!({
                "path": script,
                "watch": false,
                "state_root": tmp.path().join("plugin-state"),
            }),
        )
        .await;
        assert_eq!(installed["name"], "noop");
        assert_eq!(installed["watching"], false);

        let listed = dispatch_ok(&router, "plugin.list", serde_json::json!({})).await;
        assert_eq!(listed["plugins"].as_array().unwrap().len(), 1);

        let granted = dispatch_ok(
            &router,
            "plugin.grant",
            serde_json::json!({"plugin": "noop", "method": "pane.capture", "target": "pane-1"}),
        )
        .await;
        assert_eq!(granted["granted"], true);
        let subscribe_grant = dispatch_ok(
            &router,
            "plugin.grant",
            serde_json::json!({"plugin": "noop", "method": "pane.output.", "subscribe": true}),
        )
        .await;
        assert_eq!(subscribe_grant["subscribe"], true);

        let grants = dispatch_ok(
            &router,
            "plugin.grants",
            serde_json::json!({"plugin": "noop"}),
        )
        .await;
        assert!(
            grants["grants"]
                .as_object()
                .unwrap()
                .contains_key("pane.capture")
        );

        let audit_path = manager.audit_path("noop").await.unwrap();
        std::fs::create_dir_all(audit_path.parent().unwrap()).unwrap();
        std::fs::write(
            &audit_path,
            r#"{"seq":1,"method":"old"}
{"seq":2,"method":"new"}
"#,
        )
        .unwrap();
        let audit = dispatch_ok(
            &router,
            "plugin.audit",
            serde_json::json!({"plugin": "noop", "tail": 1}),
        )
        .await;
        assert_eq!(audit["entries"].as_array().unwrap().len(), 1);
        assert_eq!(audit["entries"][0]["method"], "new");

        let revoked = dispatch_ok(
            &router,
            "plugin.revoke",
            serde_json::json!({"plugin": "noop", "method": "pane.capture", "target": "pane-1"}),
        )
        .await;
        assert_eq!(revoked["revoked"], true);

        let reloaded = dispatch_ok(
            &router,
            "plugin.reload",
            serde_json::json!({"name": "noop"}),
        )
        .await;
        assert_eq!(reloaded["name"], "noop");

        let killed = dispatch_ok(&router, "plugin.kill", serde_json::json!({"name": "noop"})).await;
        assert_eq!(killed["killed"], "noop");
        let missing_plugin =
            dispatch_err(&router, "plugin.kill", serde_json::json!({"name": "noop"})).await;
        assert_eq!(missing_plugin.code, shux_rpc::ErrorCode::NotFound.code());
    }
}
