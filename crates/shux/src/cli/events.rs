//! `shux events …` handlers.

use crate::style;

use super::rpc::*;

/// `shux events watch [--filter ...] [--from-seq N] [--limit N]`.
///
/// Long-polls `events.watch` on a single shared connection. Each loop:
///   1. Calls `events.watch` with `from_seq` = next expected seq.
///   2. Prints every event in the response as one JSON Line on stdout.
///   3. Updates `from_seq` from the response's `next_seq`.
///   4. If `lagged: true`, prints `[STREAM_DEGRADED]` to stderr (per the
///      Codex+Gemini review — clients must know the stream dropped events).
///   5. If `gap > 0` on the first call (resumption from too-old `from_seq`),
///      prints `[GAP n]` to stderr.
///   6. Stops when `--limit N` events have been printed, or on Ctrl+C.
pub async fn handle_events_watch(
    stream: &mut tokio::net::UnixStream,
    filter: Vec<String>,
    from_seq: Option<u64>,
    timeout_ms: u64,
    limit: Option<u64>,
) -> anyhow::Result<()> {
    let mut next_seq = from_seq;
    let mut printed: u64 = 0;
    let mut first_call = true;

    loop {
        let mut params = serde_json::Map::new();
        if let Some(seq) = next_seq {
            params.insert("from_seq".into(), serde_json::json!(seq));
        }
        if !filter.is_empty() {
            params.insert("filter".into(), serde_json::json!(filter));
        }
        params.insert("timeout_ms".into(), serde_json::json!(timeout_ms));

        let result = match rpc_call(stream, "events.watch", serde_json::Value::Object(params)).await
        {
            Ok(v) => v,
            Err(e) => {
                eprintln!("{} {e}", style::error("✗ events.watch failed:"));
                return Err(anyhow::anyhow!(e));
            }
        };

        let gap = result.get("gap").and_then(|v| v.as_u64()).unwrap_or(0);
        let lagged = result
            .get("lagged")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if first_call && gap > 0 {
            eprintln!(
                "{}",
                style::warning(&format!(
                    "[GAP {gap}] resumed from a sequence older than the daemon's history; events were lost."
                ))
            );
            first_call = false;
        } else {
            first_call = false;
        }

        if lagged {
            eprintln!(
                "{}",
                style::warning(
                    "[STREAM_DEGRADED] subscriber lagged; some events were dropped by the daemon."
                )
            );
        }

        if let Some(events) = result.get("events").and_then(|v| v.as_array()) {
            for ev in events {
                println!("{}", serde_json::to_string(ev)?);
                printed += 1;
                if let Some(n) = limit
                    && printed >= n
                {
                    return Ok(());
                }
            }
        }

        if let Some(ns) = result.get("next_seq").and_then(|v| v.as_u64()) {
            next_seq = Some(ns);
        }

        // Loop unconditionally — long-poll cycles immediately when the prior
        // call returned (Codex + Gemini both warned: do NOT add an artificial
        // sleep here, it just adds latency for no benefit).
    }
}

/// `shux events history [--filter ...] [-n N]`.
pub async fn handle_events_history(
    stream: &mut tokio::net::UnixStream,
    filter: Vec<String>,
    count: u64,
) -> anyhow::Result<()> {
    let mut params = serde_json::Map::new();
    params.insert("count".into(), serde_json::json!(count));
    if !filter.is_empty() {
        params.insert("filter".into(), serde_json::json!(filter));
    }

    let result = rpc_call(stream, "events.history", serde_json::Value::Object(params)).await?;

    if let Some(events) = result.get("events").and_then(|v| v.as_array()) {
        for ev in events {
            println!("{}", serde_json::to_string(ev)?);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::test_support::*;
    use crate::cli::{args::*, plugin::*, state::*};

    #[tokio::test]
    async fn cli_events_plugin_and_apply_handlers_cover_streaming_and_permissions() {
        let event = serde_json::json!({"seq": 42, "type": "plugin.demo.tick", "data": {}});
        let (mut client, requests, task) = spawn_rpc_script(vec![
            serde_json::json!({"events": [event.clone()], "next_seq": 43, "gap": 0, "lagged": false}),
            serde_json::json!({"events": [event], "current_seq": 44}),
            serde_json::json!({"name": "demo", "version": "1.0.0", "pid": 123, "watching": true, "subscribes": ["pane."]}),
            serde_json::json!({"plugins": [{"name": "demo", "version": "1.0.0", "status": "running", "pid": 123, "uptime_ms": 2500}]}),
            serde_json::json!({"name": "demo", "pid": 124}),
            serde_json::json!({"killed": "demo"}),
            serde_json::json!({"granted": true}),
            serde_json::json!({"revoked": true}),
            serde_json::json!({"grants": {"pane.capture": "*"}, "subscribes": {"allowed": ["pane."]}}),
            serde_json::json!({"path": "/tmp/audit.jsonl", "entries": [{"ts": "now", "method": "pane.capture", "decision": "allow", "reason": "grant"}]}),
            serde_json::json!({"correlation_id": "apply-1", "outputs": [{"session_id": "sid"}], "last_event_seq": 50, "spawn_results": [{"pane_id": "pane-1", "spawned": true}]}),
        ]);

        handle_events_watch(
            &mut client,
            vec!["plugin.demo.".to_string()],
            Some(42),
            100,
            Some(1),
        )
        .await
        .unwrap();
        handle_events_history(&mut client, vec!["plugin.demo.".to_string()], 5)
            .await
            .unwrap();
        handle_plugin_install(
            &mut client,
            std::path::Path::new("/tmp/plugin"),
            &["--flag".to_string()],
            Some(std::path::Path::new("/tmp")),
            true,
            OutputFormat::Text,
        )
        .await
        .unwrap();
        handle_plugin_list(&mut client, OutputFormat::Plain)
            .await
            .unwrap();
        handle_plugin_reload(&mut client, "demo", OutputFormat::Text)
            .await
            .unwrap();
        handle_plugin_kill(&mut client, "demo", OutputFormat::Plain)
            .await
            .unwrap();
        handle_plugin_grant(
            &mut client,
            "demo",
            "pane.capture",
            Some("*"),
            false,
            OutputFormat::Plain,
        )
        .await
        .unwrap();
        handle_plugin_revoke(
            &mut client,
            "demo",
            "pane.capture",
            Some("*"),
            true,
            OutputFormat::Text,
        )
        .await
        .unwrap();
        handle_plugin_grants(&mut client, "demo", OutputFormat::Text)
            .await
            .unwrap();
        handle_plugin_audit(&mut client, "demo", 10, OutputFormat::Text)
            .await
            .unwrap();
        handle_apply(
            &mut client,
            vec![shux_core::apply::Op::CreateSession {
                name: Some("dev".to_string()),
                cwd: std::path::PathBuf::from("/tmp"),
                initial_command: Vec::new(),
                initial_window_title: None,
            }],
            false,
            std::path::Path::new("/tmp/shux.sock"),
            OutputFormat::Text,
        )
        .await
        .unwrap();

        let requests = finish_rpc_script(client, task, requests).await;
        let methods: Vec<_> = requests
            .iter()
            .map(|r| r["method"].as_str().unwrap())
            .collect();
        for method in [
            "events.watch",
            "events.history",
            "plugin.install",
            "plugin.list",
            "plugin.reload",
            "plugin.kill",
            "plugin.grant",
            "plugin.revoke",
            "plugin.grants",
            "plugin.audit",
            "state.apply",
        ] {
            assert!(methods.contains(&method), "missing RPC call {method}");
        }
        let grant = requests
            .iter()
            .find(|r| r["method"] == "plugin.grant")
            .unwrap();
        assert_eq!(grant["params"]["plugin"], "demo");
        assert_eq!(grant["params"]["method"], "pane.capture");
        let apply = requests
            .iter()
            .find(|r| r["method"] == "state.apply")
            .unwrap();
        assert_eq!(apply["params"]["ops"][0]["op"], "create_session");
    }
}
