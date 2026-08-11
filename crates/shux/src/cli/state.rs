//! `shux state apply` / `shux init` — the declarative template path.

use crate::style;

use super::{args::*, events::*, rpc::*};

pub const STARTER_TEMPLATE: &str = r#"# `shux apply review.toml` — atomic, dry-run-able with `--dry-run`.
[session]
name = "review"

[[windows]]
title = "git"
[[windows.panes]]
command = ["lazygit"]
"#;

pub fn handle_init(root: &std::path::Path, format: OutputFormat) -> anyhow::Result<()> {
    let shux_dir = root.join(".shux");
    for sub in ["templates", "scripts", "goldens", "out"] {
        std::fs::create_dir_all(shux_dir.join(sub))?;
    }

    let gitignore_path = shux_dir.join(".gitignore");
    let mut created = Vec::new();
    if !gitignore_path.exists() {
        std::fs::write(&gitignore_path, "out/\n*.log\n")?;
        created.push(gitignore_path.clone());
    }

    let template_path = shux_dir.join("templates").join("review.toml");
    let templates_dir = shux_dir.join("templates");
    let templates_empty = std::fs::read_dir(&templates_dir)
        .map(|mut it| it.next().is_none())
        .unwrap_or(true);
    if templates_empty && !template_path.exists() {
        std::fs::write(&template_path, STARTER_TEMPLATE)?;
        created.push(template_path.clone());
    }

    match format {
        OutputFormat::Json => {
            println!(
                "{}",
                crate::style::json_safe(&serde_json::to_string_pretty(&serde_json::json!({
                    "shux_dir": shux_dir.display().to_string(),
                    "created": created.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
                }))?)
            );
        }
        _ => {
            println!(
                "{} {}",
                crate::style::success("✓ scaffolded"),
                crate::style::bold(shux_dir.display().to_string().as_str()),
            );
            for path in &created {
                println!("  {} {}", crate::style::muted("+"), path.display(),);
            }
            if created.is_empty() {
                println!(
                    "  {}",
                    crate::style::muted("(already present — nothing to do)")
                );
            }
        }
    }

    Ok(())
}

/// `shux apply <template.toml>` — send the lowered ops to `state.apply`,
/// pretty-print the result, optionally hand off to `events watch` filtered
/// to the new session.
pub async fn handle_apply(
    stream: &mut tokio::net::UnixStream,
    ops: Vec<shux_core::apply::Op>,
    watch: bool,
    socket_path: &std::path::Path,
    format: OutputFormat,
) -> anyhow::Result<()> {
    if watch && matches!(format, OutputFormat::Json) {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "error": {
                    "code": -32602,
                    "message": "--watch cannot be combined with --format json",
                    "data": {
                        "detail": "state apply --watch streams human event output; omit --watch for one JSON result"
                    }
                }
            }))?
        );
        std::process::exit(2);
    }

    let params = serde_json::json!({ "ops": ops });
    let result = match rpc_call(stream, "state.apply", params).await {
        Ok(v) => v,
        Err(RpcClientError::Rpc {
            code,
            message,
            data,
        }) if matches!(format, OutputFormat::Json) => {
            let mut error = serde_json::json!({ "code": code, "message": message });
            if let Some(data) = data {
                error["data"] = data;
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({ "error": error }))?
            );
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("{} {e}", style::error("✗ apply failed:"));
            return Err(anyhow::anyhow!(e));
        }
    };

    let spawn_failures = |v: &serde_json::Value| -> usize {
        v.get("spawn_results")
            .and_then(|s| s.as_array())
            .map(|a| a.iter().filter(|s| s["spawned"] != true).count())
            .unwrap_or(0)
    };

    if matches!(format, OutputFormat::Json) {
        println!(
            "{}",
            crate::style::json_safe(&serde_json::to_string_pretty(&result)?)
        );
        // The human path already refuses to call a batch of dead panes a
        // success; returning early here left `--format json` — the format
        // scripts and agents use — exiting 0 over exactly the same batch, so
        // `shux --format json state apply t.toml && shux attach` still chained
        // into it (issue #125 follow-up).
        let failed = spawn_failures(&result);
        if failed > 0 {
            std::process::exit(1);
        }
        return Ok(());
    }

    // Summarize result for humans. correlation_id + counts on the first
    // line; per-pane spawn rows below.
    let cid = result
        .get("correlation_id")
        .and_then(|v| v.as_str())
        .unwrap_or("?");
    let outputs = result
        .get("outputs")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    let last_seq = result
        .get("last_event_seq")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let spawns: Vec<_> = result
        .get("spawn_results")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let spawned_ok = spawns.iter().filter(|s| s["spawned"] == true).count();
    let spawned_fail = spawns.len() - spawned_ok;

    // A batch that committed but whose panes never started is not a success.
    // `state.apply` deliberately does NOT roll back (codex P0 #1: killing
    // already-spawned siblings has its own side effects, so partial outcomes are
    // reported rather than undone) — but reporting them under a green tick and
    // exit code 0 let `shux state apply t.toml && shux attach` walk straight
    // into a session of dead panes (issue #125 follow-up).
    let headline = format!(
        "Applied {cid} ({} ops, {} panes spawned{}, last event seq {})",
        outputs,
        spawned_ok,
        if spawned_fail > 0 {
            format!(", {spawned_fail} failed")
        } else {
            String::new()
        },
        last_seq
    );
    if spawned_fail > 0 {
        println!("{}", style::warning(&format!("! {headline}")));
    } else {
        println!("{}", style::success(&format!("✓ {headline}")));
    }
    for s in &spawns {
        let pid = s["pane_id"].as_str().unwrap_or("?");
        let pid_short: String = pid.chars().take(8).collect();
        if s["spawned"] == true {
            println!("    {} pane {} spawned", style::success("✓"), pid_short);
        } else {
            let err = s["error"].as_str().unwrap_or("unknown error");
            println!(
                "    {} pane {} spawn failed: {}",
                style::error("✗"),
                pid_short,
                err
            );
        }
    }

    if spawned_fail > 0 && !watch {
        return Err(anyhow::anyhow!(
            "{spawned_fail} of {} pane(s) failed to spawn",
            spawns.len()
        ));
    }

    if watch {
        use crate::client;
        // Resolve the new session_id from the first output and start an
        // events.watch loop scoped to it.
        let session_id = result
            .get("outputs")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .and_then(|first| first.get("session_id"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        if let Some(_sid) = session_id {
            println!(
                "\n{}",
                style::muted(&format!(
                    "Streaming events for the new session (resume from seq {} +1)…",
                    last_seq
                ))
            );
            let mut stream2 = client::ensure_daemon_running_at(socket_path).await?;
            // Filter on the correlation_id by re-reading session events; in
            // a future PR we can add a server-side --correlation-id filter
            // for events.watch. For now: tail all events from last_seq+1 and
            // let the user Ctrl+C when they've seen enough.
            handle_events_watch(&mut stream2, vec![], Some(last_seq + 1), 5_000, None).await?;
        }
    }

    Ok(())
}
