//! `shux window …` handlers.

use crate::style;

use super::{args::*, resolve::*, rpc::*};

/// Handle the `shux window list` command.
pub async fn handle_window_list(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let session_id = resolve_session_id(stream, session_name).await?;
    let result = rpc_call(
        stream,
        "window.list",
        serde_json::json!({"session_id": session_id}),
    )
    .await?;

    match format {
        OutputFormat::Json => {
            println!(
                "{}",
                crate::style::json_safe(&serde_json::to_string_pretty(&result)?)
            );
        }
        OutputFormat::Text | OutputFormat::Plain => {
            let ctx = style::TerminalContext::detect(to_style_format(format));

            let window_infos: Vec<style::WindowInfo> = result
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .map(|w| {
                            let index =
                                w.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                            let title = w
                                .get("title")
                                .and_then(|v| v.as_str())
                                .unwrap_or("(untitled)")
                                .to_string();
                            let pane_count =
                                w.get("pane_count").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                            let is_active = w
                                .get("is_active")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false);
                            let id = w
                                .get("id")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default()
                                .to_string();
                            style::WindowInfo {
                                id,
                                title,
                                index,
                                pane_count,
                                is_active,
                            }
                        })
                        .collect()
                })
                .unwrap_or_default();

            style::render_window_list(&ctx, session_name, &window_infos);
        }
    }

    Ok(())
}

/// Handle the `shux window new` command.
#[allow(clippy::too_many_arguments)]
pub async fn handle_window_new(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    window_name: Option<String>,
    cwd: Option<std::path::PathBuf>,
    cmd: Option<String>,
    argv: Vec<String>,
    ensure: bool,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let session_id = resolve_session_id(stream, session_name).await?;

    let method = if ensure {
        "window.ensure"
    } else {
        "window.create"
    };
    let mut params = serde_json::Map::new();
    params.insert(
        "session_id".to_string(),
        serde_json::Value::String(session_id),
    );
    if let Some(name) = &window_name {
        params.insert("name".to_string(), serde_json::Value::String(name.clone()));
    }
    if let Some(c) = &cwd {
        params.insert(
            "cwd".to_string(),
            serde_json::Value::String(c.display().to_string()),
        );
    }
    // Trailing argv (after `--`) wins over --cmd, matching the
    // `shux session create` behavior so muscle memory carries over.
    //
    // `--cmd` goes out as a JSON *string* and the daemon turns it into
    // `$SHELL -c <string>`. It used to be wrapped as `["sh","-c",c]` right
    // here, which made this verb the only one whose CLI did something its RPC
    // did not, and pinned every user to `/bin/sh` rather than their own shell
    // (issue #125).
    if !argv.is_empty() {
        params.insert(
            "command".to_string(),
            serde_json::Value::Array(argv.into_iter().map(serde_json::Value::String).collect()),
        );
    } else if let Some(c) = cmd {
        params.insert("command".to_string(), serde_json::Value::String(c));
    }

    let result = rpc_call(stream, method, serde_json::Value::Object(params)).await?;

    match format {
        OutputFormat::Json => {
            println!(
                "{}",
                crate::style::json_safe(&serde_json::to_string_pretty(&result)?)
            );
        }
        OutputFormat::Text | OutputFormat::Plain => {
            let title = result
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("(untitled)");
            let index = result.get("index").and_then(|v| v.as_u64()).unwrap_or(0);
            crate::style::print_window_created(title, index);
        }
    }

    Ok(())
}

/// Handle the `shux window kill` command.
pub async fn handle_window_kill(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    window_spec: &str,
    expected_version: Option<u64>,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let session_id = resolve_session_id(stream, session_name).await?;
    let (window_id, window_title) = resolve_window_id(stream, &session_id, window_spec).await?;

    let mut params = serde_json::Map::new();
    params.insert("id".to_string(), serde_json::Value::String(window_id));
    if let Some(ev) = expected_version {
        params.insert("expected_version".to_string(), serde_json::Value::from(ev));
    }
    let result = rpc_call(stream, "window.kill", serde_json::Value::Object(params)).await?;

    match format {
        OutputFormat::Json => {
            println!(
                "{}",
                crate::style::json_safe(&serde_json::to_string_pretty(&result)?)
            );
        }
        OutputFormat::Text | OutputFormat::Plain => {
            crate::style::print_window_killed(&window_title);
        }
    }

    Ok(())
}

/// Handle the `shux window rename` command.
pub async fn handle_window_rename(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    window_spec: &str,
    new_name: &str,
    expected_version: Option<u64>,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let session_id = resolve_session_id(stream, session_name).await?;
    let (window_id, old_title) = resolve_window_id(stream, &session_id, window_spec).await?;

    let mut params = serde_json::Map::new();
    params.insert("id".to_string(), serde_json::Value::String(window_id));
    params.insert(
        "name".to_string(),
        serde_json::Value::String(new_name.to_string()),
    );
    if let Some(ev) = expected_version {
        params.insert("expected_version".to_string(), serde_json::Value::from(ev));
    }
    let result = rpc_call(stream, "window.rename", serde_json::Value::Object(params)).await?;

    match format {
        OutputFormat::Json => {
            println!(
                "{}",
                crate::style::json_safe(&serde_json::to_string_pretty(&result)?)
            );
        }
        OutputFormat::Text | OutputFormat::Plain => {
            // Report the title the daemon actually STORED, not the one we
            // asked for. The daemon sanitizes on ingress (issue #104), so
            // echoing the raw argument would both replay an escape payload
            // through this terminal and misreport what the window is now
            // called.
            let stored = result
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or(new_name);
            crate::style::print_window_renamed(&old_title, stored);
        }
    }

    Ok(())
}

/// Handle the `shux window focus` command.
pub async fn handle_window_focus(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    window_spec: &str,
    expected_version: Option<u64>,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let session_id = resolve_session_id(stream, session_name).await?;
    let (window_id, window_title) = resolve_window_id(stream, &session_id, window_spec).await?;

    let mut params = serde_json::Map::new();
    params.insert("id".to_string(), serde_json::Value::String(window_id));
    if let Some(ev) = expected_version {
        params.insert("expected_version".to_string(), serde_json::Value::from(ev));
    }
    let result = rpc_call(stream, "window.focus", serde_json::Value::Object(params)).await?;

    match format {
        OutputFormat::Json => {
            println!(
                "{}",
                crate::style::json_safe(&serde_json::to_string_pretty(&result)?)
            );
        }
        OutputFormat::Text | OutputFormat::Plain => {
            crate::style::print_window_focused(&window_title);
        }
    }

    Ok(())
}

/// Handle the `shux window reorder` command.
pub async fn handle_window_reorder(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    window_spec: &str,
    new_index: usize,
    expected_version: Option<u64>,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let session_id = resolve_session_id(stream, session_name).await?;
    let (window_id, window_title) = resolve_window_id(stream, &session_id, window_spec).await?;

    let mut params = serde_json::Map::new();
    params.insert("id".to_string(), serde_json::Value::String(window_id));
    params.insert(
        "new_index".to_string(),
        serde_json::Value::from(new_index as u64),
    );
    if let Some(ev) = expected_version {
        params.insert("expected_version".to_string(), serde_json::Value::from(ev));
    }
    let result = rpc_call(stream, "window.reorder", serde_json::Value::Object(params)).await?;

    match format {
        OutputFormat::Json => {
            println!(
                "{}",
                crate::style::json_safe(&serde_json::to_string_pretty(&result)?)
            );
        }
        OutputFormat::Text | OutputFormat::Plain => {
            crate::style::print_window_reordered(&window_title, new_index);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::pane::*;
    use crate::cli::test_support::*;

    #[tokio::test]
    async fn cli_window_and_pane_handlers_resolve_names_and_forward_params() {
        let sid = "11111111-1111-4111-8111-111111111111";
        let wid = "22222222-2222-4222-8222-222222222222";
        let pane = "33333333-3333-4333-8333-333333333333";
        let target = "44444444-4444-4444-8444-444444444444";
        let session = || session_list_response(sid, wid);
        let windows = || window_list_response(wid, pane);
        let mut responses = Vec::new();
        responses.extend([
            session(),
            serde_json::json!({"id": "new-window", "title": "editor", "index": 1}),
            session(),
            windows(),
            serde_json::json!({"killed": wid}),
            session(),
            windows(),
            serde_json::json!({"id": wid, "title": "renamed"}),
            session(),
            windows(),
            serde_json::json!({"id": wid, "previous_window_id": null}),
            session(),
            windows(),
            serde_json::json!({"id": wid, "index": 0}),
            session(),
            windows(),
            serde_json::json!([{"id": pane, "cwd": "/tmp", "command": "bash", "is_focused": true, "is_zoomed": false}]),
            windows(),
            session(),
            windows(),
            serde_json::json!({"pane": {"id": target}, "split_from": pane}),
            session(),
            windows(),
            serde_json::json!({"pane_id": pane}),
            session(),
            windows(),
            serde_json::json!({"pane_id": target}),
            session(),
            windows(),
            serde_json::json!({"pane_id": pane}),
            session(),
            windows(),
            serde_json::json!({"pane_id": pane, "is_zoomed": true}),
            session(),
            windows(),
            serde_json::json!({"pane_a": pane, "pane_b": target}),
            session(),
            windows(),
            serde_json::json!({"pane_id": pane, "title": "logs"}),
            session(),
            windows(),
            serde_json::json!({"killed": pane}),
        ]);
        let (mut client, requests, task) = spawn_rpc_script(responses);

        handle_window_new(
            &mut client,
            "dev",
            Some("editor".to_string()),
            Some(std::path::PathBuf::from("/tmp")),
            Some("echo hi".to_string()),
            vec![],
            false,
            OutputFormat::Json,
        )
        .await
        .unwrap();
        handle_window_kill(&mut client, "dev", "main", Some(3), OutputFormat::Json)
            .await
            .unwrap();
        handle_window_rename(
            &mut client,
            "dev",
            "main",
            "renamed",
            None,
            OutputFormat::Json,
        )
        .await
        .unwrap();
        handle_window_focus(&mut client, "dev", "0", None, OutputFormat::Json)
            .await
            .unwrap();
        handle_window_reorder(&mut client, "dev", "main", 0, Some(4), OutputFormat::Json)
            .await
            .unwrap();
        handle_pane_list(&mut client, "dev", Some("main"), OutputFormat::Json)
            .await
            .unwrap();
        handle_pane_split(
            &mut client,
            "dev",
            Some("main"),
            Some(pane),
            Some("horizontal"),
            Some(0.4),
            Some("echo split".to_string()),
            Vec::new(),
            OutputFormat::Json,
        )
        .await
        .unwrap();
        handle_pane_focus(&mut client, "dev", Some("main"), pane, OutputFormat::Json)
            .await
            .unwrap();
        handle_pane_focus_dir(
            &mut client,
            "dev",
            Some("main"),
            "right",
            OutputFormat::Json,
        )
        .await
        .unwrap();
        handle_pane_resize(
            &mut client,
            "dev",
            Some("main"),
            Some(pane),
            "vertical",
            Some(0.2),
            Some(9),
            OutputFormat::Json,
        )
        .await
        .unwrap();
        handle_pane_zoom(
            &mut client,
            "dev",
            Some("main"),
            Some(pane),
            None,
            OutputFormat::Json,
        )
        .await
        .unwrap();
        handle_pane_swap(
            &mut client,
            "dev",
            Some("main"),
            pane,
            target,
            Some(10),
            OutputFormat::Json,
        )
        .await
        .unwrap();
        handle_pane_title(
            &mut client,
            "dev",
            Some("main"),
            Some(pane),
            Some("logs"),
            false,
            false,
            true,
            OutputFormat::Json,
        )
        .await
        .unwrap();
        handle_pane_kill(
            &mut client,
            "dev",
            Some("main"),
            pane,
            Some(11),
            OutputFormat::Json,
        )
        .await
        .unwrap();

        let requests = finish_rpc_script(client, task, requests).await;
        let methods: Vec<_> = requests
            .iter()
            .map(|r| r["method"].as_str().unwrap())
            .collect();
        assert!(methods.contains(&"window.create"));
        assert!(methods.contains(&"window.kill"));
        assert!(methods.contains(&"pane.split"));
        assert!(methods.contains(&"pane.set_title"));

        let window_create = requests
            .iter()
            .find(|r| r["method"] == "window.create")
            .unwrap();
        // `--cmd` goes out as a STRING; the daemon is what turns it into
        // `$SHELL -c <string>`. It used to be wrapped into `["sh","-c",…]`
        // client-side, which made this the one verb whose CLI transformed a
        // parameter its RPC did not (issue #125).
        assert_eq!(
            window_create["params"]["command"],
            serde_json::json!("echo hi")
        );
        let pane_split = requests
            .iter()
            .find(|r| r["method"] == "pane.split")
            .unwrap();
        assert_eq!(pane_split["params"]["direction"], "horizontal");
        assert_eq!(pane_split["params"]["ratio"], 0.4);
        // `--cmd` reaches `pane.split` as a string, same as the other verbs.
        assert_eq!(
            pane_split["params"]["command"],
            serde_json::json!("echo split")
        );
        let pane_title = requests
            .iter()
            .find(|r| r["method"] == "pane.set_title")
            .unwrap();
        assert_eq!(pane_title["params"]["title"], "logs");
        assert_eq!(pane_title["params"]["auto"], false);
    }
}
