//! `shux session …` handlers.

use crate::style;

use super::{args::*, resolve::*, rpc::*};

/// Format a created_at timestamp as relative time.
pub fn format_created_at(value: &serde_json::Value) -> String {
    value
        .as_str()
        .map(String::from)
        .or_else(|| {
            value.as_u64().map(|ts| {
                let dt = std::time::UNIX_EPOCH + std::time::Duration::from_secs(ts);
                let elapsed = dt.elapsed().unwrap_or_default();
                if elapsed.as_secs() < 60 {
                    format!("{}s ago", elapsed.as_secs())
                } else if elapsed.as_secs() < 3600 {
                    format!("{}m ago", elapsed.as_secs() / 60)
                } else {
                    format!("{}h ago", elapsed.as_secs() / 3600)
                }
            })
        })
        .unwrap_or_else(|| "?".to_string())
}

/// Handle the `shux session list` command.
pub async fn handle_ls(
    stream: &mut tokio::net::UnixStream,
    include_scratch: bool,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let result = rpc_call(
        stream,
        "session.list",
        serde_json::json!({ "include_scratch": include_scratch }),
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

            let sessions = result
                .get("sessions")
                .and_then(|v| v.as_array())
                .or_else(|| result.as_array());

            let session_infos: Vec<style::SessionInfo> = sessions
                .map(|arr| {
                    arr.iter()
                        .map(|s| {
                            let name = s
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("(unnamed)")
                                .to_string();
                            let id = s
                                .get("id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("?")
                                .to_string();
                            let window_count = s
                                .get("windows")
                                .and_then(|v| v.as_array())
                                .map(|a| a.len())
                                .or_else(|| {
                                    s.get("window_count")
                                        .and_then(|v| v.as_u64())
                                        .map(|n| n as usize)
                                })
                                .unwrap_or(0);
                            let created = s
                                .get("created_at")
                                .map(format_created_at)
                                .unwrap_or_else(|| "?".to_string());
                            let scratch =
                                s.get("scratch").and_then(|v| v.as_bool()).unwrap_or(false);
                            style::SessionInfo {
                                name,
                                id,
                                window_count,
                                created,
                                is_active: false, // no attach tracking yet
                                scratch,
                            }
                        })
                        .collect()
                })
                .unwrap_or_default();

            style::render_session_list(&ctx, &session_infos);
        }
    }

    Ok(())
}

/// Handle the `shux session create` command.
#[derive(Debug)]
pub struct SessionCreateOptions {
    pub session_name: Option<String>,
    pub cwd: Option<std::path::PathBuf>,
    pub title: Option<String>,
    pub cmd: Option<String>,
    pub argv: Vec<String>,
    pub ensure: bool,
}

pub async fn handle_new(
    stream: &mut tokio::net::UnixStream,
    opts: SessionCreateOptions,
    format: OutputFormat,
) -> anyhow::Result<serde_json::Value> {
    let invocation_cwd = std::env::current_dir()
        .map_err(|e| anyhow::anyhow!("failed to determine current directory: {e}"))?;
    let cwd = resolve_session_create_cwd(opts.cwd, &invocation_cwd);
    let params =
        build_session_create_params(opts.session_name, cwd, opts.title, opts.cmd, opts.argv);

    let method = if opts.ensure {
        "session.ensure"
    } else {
        "session.create"
    };
    let result = rpc_call(stream, method, serde_json::Value::Object(params)).await?;

    match format {
        OutputFormat::Json => {
            println!(
                "{}",
                crate::style::json_safe(&serde_json::to_string_pretty(&result)?)
            );
        }
        OutputFormat::Text | OutputFormat::Plain => {
            let name = result
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("(unnamed)");
            let id = result.get("id").and_then(|v| v.as_str()).unwrap_or("?");
            style::print_session_created(name, id, opts.ensure);
        }
    }

    Ok(result)
}

pub fn resolve_session_create_cwd(
    cwd: Option<std::path::PathBuf>,
    invocation_cwd: &std::path::Path,
) -> std::path::PathBuf {
    let cwd = cwd.unwrap_or_else(|| invocation_cwd.to_path_buf());
    if cwd.is_absolute() {
        cwd
    } else {
        invocation_cwd.join(cwd)
    }
}

pub fn build_session_create_params(
    session_name: Option<String>,
    cwd: std::path::PathBuf,
    title: Option<String>,
    cmd: Option<String>,
    argv: Vec<String>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut params = serde_json::Map::new();
    if let Some(name) = session_name {
        params.insert("name".to_string(), serde_json::Value::String(name));
    }
    params.insert(
        "cwd".to_string(),
        serde_json::Value::String(cwd.display().to_string()),
    );
    if let Some(title) = title {
        params.insert("pane_title".to_string(), serde_json::Value::String(title));
    }
    // argv (trailing `--`) wins over --cmd if both are given.
    if !argv.is_empty() {
        params.insert(
            "command".to_string(),
            serde_json::Value::Array(argv.into_iter().map(serde_json::Value::String).collect()),
        );
    } else if let Some(command) = cmd {
        params.insert("command".to_string(), serde_json::Value::String(command));
    }
    params
}

/// Handle the `shux session kill` command.
///
/// Accepts either a session NAME or a session UUID (issue #88 direction):
/// `lens.run` returns `session_id` as a UUID, and scratch sessions are
/// excluded from the default `session.list` a name lookup would need. A
/// UUID-shaped argument resolves as an id FIRST with fallback to name
/// lookup (session names may legally be UUID-shaped; id wins when both
/// match — see `resolve_uuid_shaped_session`), then goes out as the RPC's
/// `id` param, which `session.kill` has always accepted. Plain names go
/// out as `name`, unchanged.
pub async fn handle_kill(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    expected_version: Option<u64>,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let mut params = serde_json::Map::new();
    let (key, value) = if let Ok(parsed) = uuid::Uuid::parse_str(session_name) {
        (
            "id",
            resolve_uuid_shaped_session(stream, session_name, parsed).await?,
        )
    } else if session_exists_by_name(stream, session_name).await? {
        // An exact NAME always wins over a partial id (issue #120).
        ("name", session_name.to_string())
    } else {
        // Not a name. It may still be the short id `session list` printed —
        // and `session kill` is where the documented agent loop ENDS, so it
        // has to take the same reference every other verb does. A miss here
        // reports "not found" against the name, exactly as before.
        match resolve_session_id_prefix(stream, session_name).await {
            Ok(id) => ("id", id),
            Err(RpcClientError::Rpc { code: -32004, .. }) => ("name", session_name.to_string()),
            Err(e) => return Err(e.into()),
        }
    };
    params.insert(key.to_string(), serde_json::Value::String(value));
    if let Some(ev) = expected_version {
        params.insert("expected_version".to_string(), serde_json::Value::from(ev));
    }

    let result = rpc_call(stream, "session.kill", serde_json::Value::Object(params)).await?;

    match format {
        OutputFormat::Json => {
            println!(
                "{}",
                crate::style::json_safe(&serde_json::to_string_pretty(&result)?)
            );
        }
        OutputFormat::Text | OutputFormat::Plain => {
            crate::style::print_session_killed(session_name);
        }
    }

    Ok(())
}

/// Handle the `shux session rename` command.
pub async fn handle_rename(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    new_name: &str,
    expected_version: Option<u64>,
    format: OutputFormat,
) -> anyhow::Result<()> {
    // Resolve first so `-s` takes the same reference every other verb does:
    // an exact name, a full uuid, or the short id `session list` prints.
    let target = resolve_session_id(stream, session_name).await?;
    let mut params = serde_json::Map::new();
    params.insert("id".to_string(), serde_json::Value::String(target));
    params.insert(
        "new_name".to_string(),
        serde_json::Value::String(new_name.to_string()),
    );
    if let Some(ev) = expected_version {
        params.insert("expected_version".to_string(), serde_json::Value::from(ev));
    }

    let result = rpc_call(stream, "session.rename", serde_json::Value::Object(params)).await?;

    match format {
        OutputFormat::Json => {
            println!(
                "{}",
                crate::style::json_safe(&serde_json::to_string_pretty(&result)?)
            );
        }
        OutputFormat::Text | OutputFormat::Plain => {
            crate::style::print_session_renamed(session_name, new_name);
        }
    }

    Ok(())
}

/// Handle the `shux session save` command.
pub async fn handle_session_save(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    output: Option<std::path::PathBuf>,
) -> anyhow::Result<()> {
    let target = resolve_session_id(stream, session_name).await?;
    let result = rpc_call(
        stream,
        "session.export_template",
        serde_json::json!({ "id": target }),
    )
    .await?;
    let template = result
        .get("template")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("session.export_template returned no template"))?;

    if let Some(path) = output {
        std::fs::write(&path, template)?;
        crate::style::print_success("saved", &path.display().to_string(), None);
    } else {
        print!("{template}");
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn handle_wait_for(
    stream: &mut tokio::net::UnixStream,
    session: Option<&str>,
    window: Option<&str>,
    pane: Option<&str>,
    text: Option<&str>,
    regex: Option<&str>,
    absent: bool,
    lines: u64,
    timeout_ms: u64,
    poll_ms: u64,
    format: OutputFormat,
) -> anyhow::Result<()> {
    if text.is_none() && regex.is_none() {
        anyhow::bail!("provide --text or --regex");
    }

    let mut params = serde_json::Map::new();
    if let Some(p) = pane {
        params.insert("pane_id".into(), serde_json::Value::String(p.to_string()));
    } else if let Some(s) = session {
        let sid = resolve_session_id(stream, s).await?;
        params.insert("session_id".into(), serde_json::Value::String(sid.clone()));
        if let Some(w) = window {
            let (wid, _t) = resolve_window_id(stream, &sid, w).await?;
            params.insert("window_id".into(), serde_json::Value::String(wid));
        }
    } else {
        anyhow::bail!("provide --pane or --session [--window]");
    }
    if let Some(t) = text {
        params.insert("text".into(), serde_json::Value::String(t.to_string()));
    }
    if let Some(r) = regex {
        params.insert("regex".into(), serde_json::Value::String(r.to_string()));
    }
    params.insert("absent".into(), serde_json::Value::Bool(absent));
    params.insert("lines".into(), serde_json::Value::from(lines));
    params.insert("timeout_ms".into(), serde_json::Value::from(timeout_ms));
    params.insert("poll_ms".into(), serde_json::Value::from(poll_ms));

    let result = match rpc_call(stream, "pane.wait_for", serde_json::Value::Object(params)).await {
        Ok(v) => v,
        Err(RpcClientError::Rpc {
            code,
            message,
            data,
        }) => {
            match format {
                OutputFormat::Json => {
                    let env = serde_json::json!({
                        "error": { "code": code, "message": message, "data": data }
                    });
                    println!(
                        "{}",
                        crate::style::json_safe(&serde_json::to_string_pretty(&env)?)
                    );
                }
                _ => {
                    eprintln!("{} {message}", crate::style::error("✗ wait-for:"));
                    if let Some(d) = data
                        .as_ref()
                        .and_then(|v| v.get("last_capture_preview"))
                        .and_then(|v| v.as_str())
                    {
                        eprintln!("{}", crate::style::muted("  last captured:"));
                        for line in d.lines().take(8) {
                            eprintln!("    {line}");
                        }
                    }
                }
            }
            std::process::exit(2);
        }
        Err(e) => return Err(e.into()),
    };

    match format {
        OutputFormat::Json => println!(
            "{}",
            crate::style::json_safe(&serde_json::to_string_pretty(&result)?)
        ),
        _ => {
            let elapsed = result
                .get("elapsed_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let abs = if absent { " (absent)" } else { "" };
            println!(
                "{} matched after {}ms{abs}",
                crate::style::success("✓ wait-for"),
                elapsed,
            );
        }
    }

    Ok(())
}

pub async fn handle_snapshot(
    stream: &mut tokio::net::UnixStream,
    session: Option<&str>,
    window: Option<&str>,
    output: Option<std::path::PathBuf>,
    cols: u16,
    rows: u16,
    format: OutputFormat,
) -> anyhow::Result<()> {
    use base64::Engine;

    let mut params = serde_json::Map::new();
    params.insert("cols".into(), serde_json::Value::from(cols));
    params.insert("rows".into(), serde_json::Value::from(rows));

    let method = match (session, window) {
        (Some(s), Some(w)) => {
            // Resolve --window which may be a UUID, a name, or a numeric index.
            let sid = resolve_session_id(stream, s).await?;
            let (wid, _title) = resolve_window_id(stream, &sid, w).await?;
            params.insert("session_id".into(), serde_json::Value::String(sid));
            params.insert("window_id".into(), serde_json::Value::String(wid));
            "window.snapshot"
        }
        (None, Some(w)) => {
            // No session — `w` must be a UUID (daemon resolves directly).
            params.insert("window_id".into(), serde_json::Value::String(w.to_string()));
            "window.snapshot"
        }
        (Some(s), None) => {
            let sid = resolve_session_id(stream, s).await?;
            params.insert("session_id".into(), serde_json::Value::String(sid));
            "session.snapshot"
        }
        (None, None) => {
            anyhow::bail!("provide --session and/or --window");
        }
    };

    let result = rpc_call(stream, method, serde_json::Value::Object(params)).await?;

    let b64 = result
        .get("png_base64")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("daemon response missing png_base64"))?;
    let png = base64::engine::general_purpose::STANDARD.decode(b64)?;

    match (output, format) {
        (Some(path), _) => {
            std::fs::write(&path, &png)?;
            if !matches!(format, OutputFormat::Json) {
                let w = result.get("width").and_then(|v| v.as_u64()).unwrap_or(0);
                let h = result.get("height").and_then(|v| v.as_u64()).unwrap_or(0);
                println!(
                    "{} {} ({}×{} px, {} bytes)",
                    crate::style::success("✓ snapshot →"),
                    crate::style::bold(path.display().to_string().as_str()),
                    w,
                    h,
                    png.len(),
                );
            } else {
                println!(
                    "{}",
                    crate::style::json_safe(&serde_json::to_string_pretty(&result)?)
                );
            }
        }
        (None, OutputFormat::Json) => {
            println!(
                "{}",
                crate::style::json_safe(&serde_json::to_string_pretty(&result)?)
            );
        }
        (None, _) => {
            // Default no-`--output` behaviour: print base64 to stdout so the
            // command is pipe-/jq-friendly and never dumps binary control
            // bytes into a TTY. Use `--output -.png > frame.png` (or just
            // `--output frame.png`) for raw bytes.
            println!("{b64}");
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::system::*;
    use crate::cli::test_support::*;

    #[test]
    fn test_resolve_session_create_cwd_defaults_to_invocation_cwd() {
        let cwd = resolve_session_create_cwd(None, std::path::Path::new("/tmp/shux-demo"));

        assert_eq!(cwd, std::path::PathBuf::from("/tmp/shux-demo"));
    }

    #[test]
    fn test_resolve_session_create_cwd_absolutizes_relative_override() {
        let cwd = resolve_session_create_cwd(
            Some(std::path::PathBuf::from("nested/project")),
            std::path::Path::new("/tmp/shux-demo"),
        );

        assert_eq!(
            cwd,
            std::path::PathBuf::from("/tmp/shux-demo/nested/project")
        );
    }

    #[test]
    fn test_resolve_session_create_cwd_preserves_absolute_override() {
        let cwd = resolve_session_create_cwd(
            Some(std::path::PathBuf::from("/var/tmp/shux-project")),
            std::path::Path::new("/tmp/shux-demo"),
        );

        assert_eq!(cwd, std::path::PathBuf::from("/var/tmp/shux-project"));
    }

    #[test]
    fn test_build_session_create_params_always_includes_cwd() {
        let params = build_session_create_params(
            Some("demo".to_string()),
            std::path::PathBuf::from("/tmp/shux-demo"),
            Some("aww-shux".to_string()),
            None,
            vec!["pwd".to_string()],
        );

        assert_eq!(params.get("name").and_then(|v| v.as_str()), Some("demo"));
        assert_eq!(
            params.get("cwd").and_then(|v| v.as_str()),
            Some("/tmp/shux-demo")
        );
        assert_eq!(
            params.get("pane_title").and_then(|v| v.as_str()),
            Some("aww-shux")
        );
        assert_eq!(params.get("command"), Some(&serde_json::json!(["pwd"])));
    }

    // ── issue #125: what `--cmd` and trailing argv put on the wire ────
    //
    // The CLI does not transform either one. `--cmd` travels as a JSON string
    // (the daemon runs it through `$SHELL -c`), trailing argv travels as an
    // array (exec'd directly). Anything else here would be a CLI-only
    // behaviour the RPC does not have.

    #[test]
    fn cmd_is_sent_as_a_string_never_pre_split() {
        let params = build_session_create_params(
            None,
            std::path::PathBuf::from("/tmp"),
            None,
            Some("printf 'X\n'; sleep 300".to_string()),
            Vec::new(),
        );
        assert_eq!(
            params.get("command"),
            Some(&serde_json::json!("printf 'X\n'; sleep 300"))
        );
    }

    #[test]
    fn trailing_argv_is_sent_as_an_array_and_beats_cmd() {
        let params = build_session_create_params(
            None,
            std::path::PathBuf::from("/tmp"),
            None,
            Some("echo from-cmd".to_string()),
            vec!["nvim".to_string(), "a b.rs".to_string()],
        );
        assert_eq!(
            params.get("command"),
            Some(&serde_json::json!(["nvim", "a b.rs"]))
        );
    }

    #[test]
    fn no_cmd_and_no_argv_sends_no_command_at_all() {
        let params = build_session_create_params(
            None,
            std::path::PathBuf::from("/tmp"),
            None,
            None,
            Vec::new(),
        );
        assert!(params.get("command").is_none());
    }

    #[tokio::test]
    async fn cli_session_handlers_emit_expected_rpc_shapes() {
        let sid = "11111111-1111-4111-8111-111111111111";
        let wid = "22222222-2222-4222-8222-222222222222";
        let template = "[session]\nname = \"dev\"\n";
        // `kill`, `rename` and `save` each resolve `-s` first now (issue #120),
        // so each is preceded by its own `session.list`.
        let (mut client, requests, task) = spawn_rpc_script(vec![
            session_list_response(sid, wid),
            serde_json::json!({"id": sid, "name": "dev", "created": true}),
            session_list_response(sid, wid), // kill: is "dev" a NAME?
            serde_json::json!({"killed": "dev"}),
            session_list_response(sid, wid), // rename: resolve -s
            serde_json::json!({"id": sid, "name": "renamed"}),
            session_list_response(sid, wid), // save: resolve -s
            serde_json::json!({"template": template}),
            serde_json::json!({"version": "0.26.0", "git_sha": "abc123"}),
        ]);

        handle_ls(&mut client, false, OutputFormat::Json)
            .await
            .unwrap();
        let created = handle_new(
            &mut client,
            SessionCreateOptions {
                session_name: Some("dev".to_string()),
                cwd: Some(std::path::PathBuf::from("relative")),
                title: Some("agent".to_string()),
                cmd: Some("ignored".to_string()),
                argv: vec!["vim".to_string(), "main.rs".to_string()],
                ensure: true,
            },
            OutputFormat::Json,
        )
        .await
        .unwrap();
        assert_eq!(created["created"], true);
        handle_kill(&mut client, "dev", Some(7), OutputFormat::Json)
            .await
            .unwrap();
        handle_rename(&mut client, "dev", "renamed", Some(8), OutputFormat::Json)
            .await
            .unwrap();
        handle_session_save(&mut client, "dev", None).await.unwrap();
        handle_version(&mut client, OutputFormat::Json)
            .await
            .unwrap();

        let requests = finish_rpc_script(client, task, requests).await;
        assert_eq!(requests[0]["method"], "session.list");
        assert_eq!(requests[1]["method"], "session.ensure");
        assert_eq!(requests[1]["params"]["name"], "dev");
        assert!(
            requests[1]["params"]["cwd"]
                .as_str()
                .unwrap()
                .ends_with("relative")
        );
        assert_eq!(requests[1]["params"]["pane_title"], "agent");
        assert_eq!(
            requests[1]["params"]["command"],
            serde_json::json!(["vim", "main.rs"])
        );
        assert_eq!(requests[2]["method"], "session.list");
        assert_eq!(requests[3]["method"], "session.kill");
        // An exact NAME still wins, so kill targets by name, not by id.
        assert_eq!(requests[3]["params"]["name"], "dev");
        assert_eq!(requests[3]["params"]["expected_version"], 7);
        assert_eq!(requests[4]["method"], "session.list");
        assert_eq!(requests[5]["method"], "session.rename");
        assert_eq!(requests[5]["params"]["new_name"], "renamed");
        // …while rename and save resolve to the id, so a short id works there.
        assert_eq!(requests[5]["params"]["id"], sid);
        assert_eq!(requests[6]["method"], "session.list");
        assert_eq!(requests[7]["method"], "session.export_template");
        assert_eq!(requests[7]["params"]["id"], sid);
        assert_eq!(requests[8]["method"], "system.version");
    }
}
