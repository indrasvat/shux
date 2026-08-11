//! `shux plugin …` handlers.

use crate::style;

use super::{args::*, rpc::*};

use crate::features::plugin::{PluginScaffoldRuntime, ScaffoldOptions};

/// `shux plugin install <path>` — register a plugin executable
/// with the daemon. Spawns a `plugin.install` RPC and reports the
/// resolved manifest.
/// Resolve the per-install plugin-state root from a starting cwd.
/// Walks up looking for an existing `.shux/` ancestor (so a plugin
/// installed from a subdirectory of a project still lands its state
/// in the project's `.shux/plugins/`). Falls back to anchoring at
/// the cwd itself when no `.shux/` is found in any ancestor.
pub fn resolve_plugin_state_root(start: &std::path::Path) -> std::path::PathBuf {
    let mut cur = start;
    loop {
        if cur.join(".shux").is_dir() {
            return cur.join(".shux").join("plugins");
        }
        match cur.parent() {
            Some(p) => cur = p,
            None => break,
        }
    }
    start.join(".shux").join("plugins")
}

pub fn handle_plugin_scaffold(
    path: &std::path::Path,
    runtime: PluginScaffoldRuntime,
    name: Option<String>,
    id: Option<String>,
    force: bool,
    format: OutputFormat,
) -> anyhow::Result<()> {
    use crate::features::plugin;

    let report = plugin::scaffold_plugin(
        path,
        &ScaffoldOptions {
            runtime,
            name,
            id,
            force,
        },
    )?;

    match format {
        OutputFormat::Json => {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "root": report.root,
                    "name": report.name,
                    "id": report.id,
                    "runtime": runtime.as_str(),
                    "entrypoint": report.entrypoint,
                }))?
            );
        }
        OutputFormat::Plain => {
            println!(
                "{}\t{}\t{}\t{}",
                report.name,
                report.id,
                runtime.as_str(),
                report.root.display()
            );
        }
        OutputFormat::Text => {
            println!(
                "{} {} {}",
                style::success("✓ scaffolded plugin"),
                style::bold(&report.name),
                style::muted(&format!("at {}", report.root.display())),
            );
            println!(
                "  {} {}",
                style::muted("entrypoint"),
                report.entrypoint.display()
            );
        }
    }
    Ok(())
}

pub async fn handle_plugin_install(
    stream: &mut tokio::net::UnixStream,
    path: &std::path::Path,
    args: &[String],
    cwd: Option<&std::path::Path>,
    watch: bool,
    format: OutputFormat,
) -> anyhow::Result<()> {
    use crate::features::plugin;

    let resolved = plugin::resolve_plugin_package(path)?;
    let mut resolved_args = resolved.args;
    resolved_args.extend(args.iter().cloned());

    let mut params = serde_json::Map::new();
    params.insert(
        "path".into(),
        serde_json::Value::String(resolved.command.display().to_string()),
    );
    if !resolved_args.is_empty() {
        params.insert("args".into(), serde_json::json!(resolved_args));
    }
    let resolved_cwd = cwd.map(std::path::Path::to_path_buf).or(resolved.cwd);
    if let Some(cwd) = resolved_cwd.as_deref() {
        params.insert(
            "cwd".into(),
            serde_json::Value::String(cwd.display().to_string()),
        );
    }
    if let Some(expected_name) = resolved.expected_name {
        params.insert(
            "expected_name".into(),
            serde_json::Value::String(expected_name),
        );
    }
    if let Some(expected_version) = resolved.expected_version {
        params.insert(
            "expected_version".into(),
            serde_json::Value::String(expected_version),
        );
    }
    params.insert("watch".into(), serde_json::Value::Bool(watch));

    // Pin the plugin's persisted-state root to the CLIENT's cwd so a
    // daemon shared across multiple project checkouts keeps each
    // project's plugin state isolated (codex P2 review on PR #32).
    // Walks up from cwd to find an existing `.shux/` ancestor; if
    // none found, anchors at the cwd itself. The daemon creates the
    // `<state_root>/<plugin_name>/` dir lazily on first `state.set`.
    if let Ok(cwd) = std::env::current_dir() {
        let state_root = resolve_plugin_state_root(&cwd);
        params.insert(
            "state_root".into(),
            serde_json::Value::String(state_root.display().to_string()),
        );
    }

    let result = rpc_call(stream, "plugin.install", serde_json::Value::Object(params)).await?;

    match format {
        OutputFormat::Json => println!(
            "{}",
            crate::style::json_safe(&serde_json::to_string_pretty(&result)?)
        ),
        OutputFormat::Plain => {
            let name = &crate::style::safe_label(
                result.get("name").and_then(|v| v.as_str()).unwrap_or("?"),
            );
            let ver = result
                .get("version")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let pid = result.get("pid").and_then(|v| v.as_u64()).unwrap_or(0);
            println!("{name}\t{ver}\t{pid}");
        }
        OutputFormat::Text => {
            let name = &crate::style::safe_label(
                result.get("name").and_then(|v| v.as_str()).unwrap_or("?"),
            );
            let ver = result
                .get("version")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let pid = result.get("pid").and_then(|v| v.as_u64()).unwrap_or(0);
            let watching = result
                .get("watching")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let subs: Vec<String> = result
                .get("subscribes")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let sub_str = if subs.is_empty() {
                String::from("∅")
            } else {
                subs.join(",")
            };
            let watch_str = if watching { ", watching" } else { "" };
            // Strip a leading "v" the plugin manifest may have
            // already supplied so we don't end up with "vv1".
            let display_ver = ver.strip_prefix('v').unwrap_or(ver);
            println!(
                "{} {} {} (pid {}, subscribes: {}{})",
                style::success("✓ installed plugin"),
                style::bold(name),
                style::muted(&format!("v{display_ver}")),
                pid,
                style::muted(&sub_str),
                style::muted(watch_str),
            );
        }
    }
    Ok(())
}

/// `shux plugin reload <name>` — manual hot-reload tick. The daemon
/// kills + respawns the plugin from the same source. Equivalent to
/// what the file watcher does automatically when `--no-watch` was
/// not passed.
pub async fn handle_plugin_reload(
    stream: &mut tokio::net::UnixStream,
    name: &str,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let params = serde_json::json!({ "name": name });
    let result = rpc_call(stream, "plugin.reload", params).await?;

    match format {
        OutputFormat::Json => println!(
            "{}",
            crate::style::json_safe(&serde_json::to_string_pretty(&result)?)
        ),
        OutputFormat::Plain => {
            let pid = result.get("pid").and_then(|v| v.as_u64()).unwrap_or(0);
            println!("{}\t{pid}", crate::style::safe_label(name));
        }
        OutputFormat::Text => {
            let pid = result.get("pid").and_then(|v| v.as_u64()).unwrap_or(0);
            println!(
                "{} {} (pid {})",
                style::success("✓ reloaded plugin"),
                style::bold(name),
                pid,
            );
        }
    }
    Ok(())
}

/// `shux plugin list` — print every running plugin in a small box.
pub async fn handle_plugin_list(
    stream: &mut tokio::net::UnixStream,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let result = rpc_call(stream, "plugin.list", serde_json::json!({})).await?;
    let plugins = result
        .get("plugins")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    match format {
        OutputFormat::Json => {
            println!(
                "{}",
                crate::style::json_safe(&serde_json::to_string_pretty(&result)?)
            );
        }
        OutputFormat::Plain => {
            for p in &plugins {
                let name = &crate::style::safe_label(
                    p.get("name").and_then(|v| v.as_str()).unwrap_or("?"),
                );
                let ver = p.get("version").and_then(|v| v.as_str()).unwrap_or("?");
                let status = p.get("status").and_then(|v| v.as_str()).unwrap_or("?");
                let pid = p.get("pid").and_then(|v| v.as_u64()).unwrap_or(0);
                let up_ms = p.get("uptime_ms").and_then(|v| v.as_u64()).unwrap_or(0);
                println!("{name}\t{ver}\t{status}\t{pid}\t{up_ms}");
            }
        }
        OutputFormat::Text => {
            if plugins.is_empty() {
                println!("{}", style::muted("no plugins installed"));
                return Ok(());
            }
            println!("{}", style::muted(&format!("{} plugin(s)", plugins.len())));
            for p in &plugins {
                let name = &crate::style::safe_label(
                    p.get("name").and_then(|v| v.as_str()).unwrap_or("?"),
                );
                let ver = p.get("version").and_then(|v| v.as_str()).unwrap_or("?");
                let status = p.get("status").and_then(|v| v.as_str()).unwrap_or("?");
                let pid = p.get("pid").and_then(|v| v.as_u64()).unwrap_or(0);
                let up_ms = p.get("uptime_ms").and_then(|v| v.as_u64()).unwrap_or(0);
                let up_s = up_ms / 1000;
                let dot = if status == "running" {
                    style::success("●").to_string()
                } else {
                    style::warning("○").to_string()
                };
                println!(
                    "  {} {} {} {} (pid {}, up {}s)",
                    dot,
                    style::bold(name),
                    style::muted(&format!("v{ver}")),
                    style::muted(status),
                    pid,
                    up_s
                );
            }
        }
    }
    Ok(())
}

/// `shux plugin kill <name>` — send shutdown + reap.
pub async fn handle_plugin_kill(
    stream: &mut tokio::net::UnixStream,
    name: &str,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let params = serde_json::json!({ "name": name });
    let result = rpc_call(stream, "plugin.kill", params).await?;

    match format {
        OutputFormat::Json => println!(
            "{}",
            crate::style::json_safe(&serde_json::to_string_pretty(&result)?)
        ),
        OutputFormat::Plain => println!("{}\tkilled", crate::style::safe_label(name)),
        OutputFormat::Text => println!(
            "{} {}",
            style::success("✓ killed plugin"),
            style::bold(style::safe_label(name))
        ),
    }
    Ok(())
}

/// `shux plugin stop <name>` — UX alias for graceful shutdown + unregister.
pub async fn handle_plugin_stop(
    stream: &mut tokio::net::UnixStream,
    name: &str,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let params = serde_json::json!({ "name": name });
    let result = rpc_call(stream, "plugin.kill", params).await?;

    match format {
        OutputFormat::Json => println!(
            "{}",
            crate::style::json_safe(&serde_json::to_string_pretty(&result)?)
        ),
        OutputFormat::Plain => println!("{}\tstopped", crate::style::safe_label(name)),
        OutputFormat::Text => println!(
            "{} {}",
            style::success("✓ stopped plugin"),
            style::bold(style::safe_label(name))
        ),
    }
    Ok(())
}

pub async fn handle_plugin_grant(
    stream: &mut tokio::net::UnixStream,
    plugin: &str,
    method: &str,
    target: Option<&str>,
    subscribe: bool,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let mut params = serde_json::Map::new();
    params.insert("plugin".into(), plugin.into());
    params.insert("method".into(), method.into());
    if let Some(t) = target {
        params.insert("target".into(), t.into());
    }
    if subscribe {
        params.insert("subscribe".into(), true.into());
    }
    let result = rpc_call(stream, "plugin.grant", serde_json::Value::Object(params)).await?;
    match format {
        OutputFormat::Json => println!(
            "{}",
            crate::style::json_safe(&serde_json::to_string_pretty(&result)?)
        ),
        OutputFormat::Plain => {
            let scope = target.unwrap_or("*");
            let kind = if subscribe { "subscribe" } else { "method" };
            println!("{plugin}\t{kind}\t{method}\t{scope}\tgranted");
        }
        OutputFormat::Text => {
            let scope = target.map(|t| format!(" → {t}")).unwrap_or_default();
            let kind = if subscribe { " (subscribe)" } else { "" };
            println!(
                "{} {} {} {}{}",
                style::success("✓ granted"),
                style::bold(plugin),
                style::accent(method),
                style::muted(&scope),
                kind
            );
        }
    }
    Ok(())
}

pub async fn handle_plugin_revoke(
    stream: &mut tokio::net::UnixStream,
    plugin: &str,
    method: &str,
    target: Option<&str>,
    subscribe: bool,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let mut params = serde_json::Map::new();
    params.insert("plugin".into(), plugin.into());
    params.insert("method".into(), method.into());
    if let Some(t) = target {
        params.insert("target".into(), t.into());
    }
    if subscribe {
        params.insert("subscribe".into(), true.into());
    }
    let result = rpc_call(stream, "plugin.revoke", serde_json::Value::Object(params)).await?;
    match format {
        OutputFormat::Json => println!(
            "{}",
            crate::style::json_safe(&serde_json::to_string_pretty(&result)?)
        ),
        OutputFormat::Plain => {
            let scope = target.unwrap_or("*");
            let kind = if subscribe { "subscribe" } else { "method" };
            println!("{plugin}\t{kind}\t{method}\t{scope}\trevoked");
        }
        OutputFormat::Text => {
            let scope = target.map(|t| format!(" → {t}")).unwrap_or_default();
            let kind = if subscribe { " (subscribe)" } else { "" };
            println!(
                "{} {} {} {}{}",
                style::success("✓ revoked"),
                style::bold(plugin),
                style::accent(method),
                style::muted(&scope),
                kind
            );
        }
    }
    Ok(())
}

pub async fn handle_plugin_grants(
    stream: &mut tokio::net::UnixStream,
    plugin: &str,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let params = serde_json::json!({"plugin": plugin});
    let result = rpc_call(stream, "plugin.grants", params).await?;
    match format {
        OutputFormat::Json => println!(
            "{}",
            crate::style::json_safe(&serde_json::to_string_pretty(&result)?)
        ),
        OutputFormat::Plain => {
            if let Some(g) = result.get("grants").and_then(|v| v.as_object()) {
                for (method, scope) in g {
                    let scope_str = match scope {
                        serde_json::Value::String(s) => s.clone(),
                        serde_json::Value::Array(a) => a
                            .iter()
                            .filter_map(|v| v.as_str())
                            .collect::<Vec<_>>()
                            .join(","),
                        _ => "?".into(),
                    };
                    println!("grant\t{method}\t{scope_str}");
                }
            }
            if let Some(allowed) = result
                .get("subscribes")
                .and_then(|s| s.get("allowed"))
                .and_then(|a| a.as_array())
            {
                for f in allowed.iter().filter_map(|v| v.as_str()) {
                    println!("subscribe\t{f}");
                }
            }
        }
        OutputFormat::Text => {
            println!("{} {}", style::accent("plugin"), style::bold(plugin));
            let g_map = result.get("grants").and_then(|v| v.as_object());
            let empty = g_map.map(|m| m.is_empty()).unwrap_or(true);
            if empty {
                println!("  {}", style::muted("(no grants)"));
            } else if let Some(g) = g_map {
                println!("  {}", style::bold("methods:"));
                for (method, scope) in g {
                    let scope_str = match scope {
                        serde_json::Value::String(s) => s.clone(),
                        serde_json::Value::Array(a) => format!(
                            "[{}]",
                            a.iter()
                                .filter_map(|v| v.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                        _ => "?".into(),
                    };
                    println!(
                        "    {} → {}",
                        style::accent(method),
                        style::muted(&scope_str)
                    );
                }
            }
            if let Some(allowed) = result
                .get("subscribes")
                .and_then(|s| s.get("allowed"))
                .and_then(|a| a.as_array())
                && !allowed.is_empty()
            {
                println!("  {}", style::bold("subscribes:"));
                for f in allowed.iter().filter_map(|v| v.as_str()) {
                    println!("    {}", style::accent(f));
                }
            }
        }
    }
    Ok(())
}

pub async fn handle_plugin_audit(
    stream: &mut tokio::net::UnixStream,
    plugin: &str,
    tail: usize,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let params = serde_json::json!({"plugin": plugin, "tail": tail});
    let result = rpc_call(stream, "plugin.audit", params).await?;
    match format {
        OutputFormat::Json => println!(
            "{}",
            crate::style::json_safe(&serde_json::to_string_pretty(&result)?)
        ),
        OutputFormat::Plain => {
            if let Some(entries) = result.get("entries").and_then(|v| v.as_array()) {
                for e in entries {
                    println!("{}", serde_json::to_string(e)?);
                }
            }
        }
        OutputFormat::Text => {
            let path = result
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("<unknown>");
            println!("{} {}", style::muted("audit log:"), style::muted(path));
            let entries = result
                .get("entries")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            if entries.is_empty() {
                println!("  {}", style::muted("(empty)"));
            }
            for e in entries {
                let ts = e.get("ts").and_then(|v| v.as_str()).unwrap_or("?");
                let m = e.get("method").and_then(|v| v.as_str()).unwrap_or("?");
                let d = e.get("decision").and_then(|v| v.as_str()).unwrap_or("?");
                let r = e.get("reason").and_then(|v| v.as_str()).unwrap_or("?");
                let stamp = style::muted(ts);
                let method = style::accent(m);
                let decision = if d == "allow" {
                    style::success(d).to_string()
                } else {
                    style::error(d).to_string()
                };
                println!("  {} {} {} {}", stamp, decision, method, style::muted(r));
            }
        }
    }
    Ok(())
}
