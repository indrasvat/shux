//! Turning what a user typed into an id the daemon accepts.
//!
//! Names, full UUIDs, id prefixes and window indices all arrive through the
//! same flags, and the precedence between them is the whole subtlety: an exact
//! name always beats a partial id.

use super::rpc::*;

/// Resolve a UUID-SHAPED session argument against the live session list:
/// id resolution FIRST (the common case — ids come from `lens.run` /
/// `session create` responses), falling back to NAME lookup when no session
/// has that id (session names may legally be UUID-shaped strings; codex P6
/// round-1 major 1 — a pure id short-circuit made such names unaddressable
/// and could mistarget). Precedence when the arg matches BOTH a real id and
/// a different session's name: the id wins, with a warning on stderr (the
/// ambiguity is cheaply detectable here since the list is already in hand).
/// When the arg matches NOTHING, its NORMALIZED form is passed through as
/// an id so the server produces its canonical not-found error.
///
/// `parsed` is the arg's parse (claude P6 round-1 extra: `Uuid::parse_str`
/// also accepts the 32-hex SIMPLE form and uppercase — session ids
/// serialize hyphenated lowercase, so the id comparison MUST go through the
/// normalized `to_string()` form, never raw string equality; the NAME
/// comparison stays raw/exact because names are arbitrary strings).
///
/// Queries with `include_scratch: true` because `lens.run` ids target
/// hidden scratch sessions (visibility for listing is not authorization to
/// act on a known id — LENS-R-041 principle).
pub async fn resolve_uuid_shaped_session(
    stream: &mut tokio::net::UnixStream,
    arg: &str,
    parsed: uuid::Uuid,
) -> Result<String, RpcClientError> {
    // Canonical hyphenated-lowercase form — what session ids serialize as.
    let normalized = parsed.to_string();
    let result = rpc_call(
        stream,
        "session.list",
        serde_json::json!({ "include_scratch": true }),
    )
    .await?;
    let sessions = result
        .get("sessions")
        .and_then(|v| v.as_array())
        .or_else(|| result.as_array());

    let mut id_match = false;
    let mut name_match_id: Option<String> = None;
    if let Some(sessions) = sessions {
        for s in sessions {
            let sid = s.get("id").and_then(|v| v.as_str());
            if sid == Some(normalized.as_str()) {
                id_match = true;
            }
            if s.get("name").and_then(|v| v.as_str()) == Some(arg)
                && let Some(sid) = sid
            {
                name_match_id = Some(sid.to_string());
            }
        }
    }

    match (id_match, name_match_id) {
        (true, Some(name_id)) if name_id != normalized => {
            eprintln!(
                "{}",
                crate::style::warning(format!(
                    "warning: '{arg}' matches both a session ID and a different \
                     session's NAME; targeting the session with that ID (id wins). \
                     To target the session named '{arg}', pass its id: {name_id}"
                ))
            );
            Ok(normalized)
        }
        (true, _) => Ok(normalized),
        (false, Some(name_id)) => Ok(name_id),
        // No match either way: pass the normalized form through as an id so
        // the server emits its canonical not-found (clean, consistent).
        (false, None) => Ok(normalized),
    }
}

/// Resolve a session name-or-UUID to its UUID.
///
/// Accepts either form (issue #88: RPC methods already take `id` OR `name` —
/// `-s/--session` only resolved by name, so a caller holding a session UUID
/// straight from an RPC/CLI result — e.g. `lens.run`'s `session_id`, which
/// targets a SCRATCH session excluded from the default `session.list` —
/// had no CLI-side way to address it). UUID-shaped input (hyphenated OR
/// 32-hex simple form, any case — everything `Uuid::parse_str` accepts)
/// resolves as an id FIRST with name fallback; id wins when both match
/// (see `resolve_uuid_shaped_session` for the precedence rules). Non-UUID
/// input resolves by name as always.
pub async fn resolve_session_id(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
) -> Result<String, RpcClientError> {
    if let Ok(parsed) = uuid::Uuid::parse_str(session_name) {
        return resolve_uuid_shaped_session(stream, session_name, parsed).await;
    }
    let result = rpc_call(stream, "session.list", serde_json::json!({})).await?;
    let sessions = result
        .get("sessions")
        .and_then(|v| v.as_array())
        .or_else(|| result.as_array());

    if let Some(sessions) = sessions {
        for s in sessions {
            if s.get("name").and_then(|v| v.as_str()) == Some(session_name)
                && let Some(id) = s.get("id").and_then(|v| v.as_str())
            {
                return Ok(id.to_string());
            }
        }
    }

    // No session by that NAME. Before giving up, try it as an id prefix —
    // `session list` prints the 8-char short id in its own last column, and
    // before issue #120 that column was decorative (issue #120). An exact
    // name always wins over a partial id, which is why this runs second and
    // never changes the outcome of a call that already worked.
    //
    // `include_scratch` mirrors the full-uuid path: a caller holding an id
    // fragment for a hidden scratch session may still act on it — visibility
    // is not authorization (LENS-R-041).
    resolve_session_id_prefix(stream, session_name).await
}

/// Does a session with exactly this NAME exist?
///
/// `include_scratch` so a hidden lens session is visible here: an exact name
/// is an exact match either way, and letting the prefix pass claim it instead
/// would be a silent mistarget.
pub async fn session_exists_by_name(
    stream: &mut tokio::net::UnixStream,
    name: &str,
) -> Result<bool, RpcClientError> {
    let result = rpc_call(
        stream,
        "session.list",
        serde_json::json!({ "include_scratch": true }),
    )
    .await?;
    let sessions = result
        .get("sessions")
        .and_then(|v| v.as_array())
        .or_else(|| result.as_array())
        .cloned()
        .unwrap_or_default();
    Ok(sessions
        .iter()
        .any(|s| s.get("name").and_then(|v| v.as_str()) == Some(name)))
}

/// Resolve a non-UUID, non-name session argument as an id prefix.
///
/// Split out so both this and `handle_kill`'s id/name routing share one
/// definition of "what a short session id means".
pub async fn resolve_session_id_prefix(
    stream: &mut tokio::net::UnixStream,
    arg: &str,
) -> Result<String, RpcClientError> {
    use shux_core::idref::{RefKind, parse_ref};

    let not_found = || RpcClientError::Rpc {
        code: -32004,
        message: format!("session '{arg}' not found"),
        data: None,
    };

    // Anything that is not prefix-shaped keeps the old "not found" wording:
    // for a name that simply does not exist, "not a hex prefix" would be a
    // confusing thing to say.
    let Ok(shux_core::idref::ParsedRef::Prefix(prefix)) = parse_ref(RefKind::Session, arg) else {
        return Err(not_found());
    };

    let result = rpc_call(
        stream,
        "session.list",
        serde_json::json!({ "include_scratch": true }),
    )
    .await?;
    let sessions = result
        .get("sessions")
        .and_then(|v| v.as_array())
        .or_else(|| result.as_array())
        .cloned()
        .unwrap_or_default();

    let mut hits: Vec<String> = sessions
        .iter()
        .filter_map(|s| s.get("id").and_then(|v| v.as_str()))
        .filter(|id| {
            id.replace('-', "")
                .to_ascii_lowercase()
                .starts_with(&prefix)
        })
        .map(|id| id.to_string())
        .collect();
    hits.sort();

    match hits.len() {
        0 => Err(not_found()),
        1 => Ok(hits.remove(0)),
        _ => Err(ambiguous_ref_error("session", arg, &hits, None)),
    }
}

/// Build the client-side twin of the daemon's ambiguous-reference error
/// (`RpcError::ambiguous_ref`).
///
/// Same code, same `data` shape — so a script sees one contract whether the
/// collision was detected in the CLI (which resolves `-s` / `-w` against a
/// listing before it sends anything) or in the daemon. Carrying `detail` also
/// keeps the rendered message clean: without it the display falls back to
/// "RPC error -32602: <message>", and the code number is noise on an error a
/// person is meant to act on.
pub fn ambiguous_ref_error(
    resource: &str,
    id: &str,
    candidates: &[String],
    scope: Option<&str>,
) -> RpcClientError {
    let total = candidates.len();
    let listed: Vec<String> = candidates
        .iter()
        .take(shux_core::idref::MAX_LISTED_CANDIDATES)
        .cloned()
        .collect();
    let ellipsis = if total > listed.len() { ", …" } else { "" };
    let where_ = scope
        .map(|s| format!(" in session {s}"))
        .unwrap_or_default();
    let detail = format!(
        "{resource} id '{id}' is ambiguous: {total} {resource}s{where_} share that \
         prefix ({}{ellipsis}). Use more characters.",
        listed.join(", ")
    );
    RpcClientError::Rpc {
        code: -32602,
        message: detail.clone(),
        data: Some(serde_json::json!({
            "detail": detail,
            "resource": resource,
            "id": id,
            "candidates": listed,
            "total": total,
            "hint": "Pass more characters of the id, or the full uuid",
        })),
    }
}

/// Resolve a window specifier (name or index) to (window_id, window_title).
pub async fn resolve_window_id(
    stream: &mut tokio::net::UnixStream,
    session_id: &str,
    window_spec: &str,
) -> Result<(String, String), RpcClientError> {
    let result = rpc_call(
        stream,
        "window.list",
        serde_json::json!({"session_id": session_id}),
    )
    .await?;
    let windows = result.as_array().ok_or_else(|| RpcClientError::Rpc {
        code: -32603,
        message: "unexpected response from window.list".to_string(),
        data: None,
    })?;

    // Try as numeric index first
    if let Ok(idx) = window_spec.parse::<usize>()
        && let Some(w) = windows.get(idx)
    {
        let id = w.get("id").and_then(|v| v.as_str()).unwrap_or("?");
        let title = w.get("title").and_then(|v| v.as_str()).unwrap_or("?");
        return Ok((id.to_string(), title.to_string()));
    }

    // Try as window name. Titles are stored sanitized (issue #104), so
    // normalize the selector the same way — the operator types what they
    // see in `window list`, but a script may pass the raw value straight
    // from the template. Both have to land on the same window.
    let wanted = shux_core::model::sanitize_title(window_spec);
    for w in windows {
        if w.get("title").and_then(|v| v.as_str()) == Some(wanted.as_str()) {
            let id = w.get("id").and_then(|v| v.as_str()).unwrap_or("?");
            let title = w.get("title").and_then(|v| v.as_str()).unwrap_or("?");
            return Ok((id.to_string(), title.to_string()));
        }
    }

    // Finally, as an id — full UUID or the short form `window list` prints.
    // `--window`'s help has always said "window id or index"; before issue
    // #120 the id half of that promise was not implemented, so a UUID read
    // out of `window list --format json` was rejected. Index and title are
    // tried first, so no spec that resolved before resolves differently now.
    resolve_window_id_by_id(windows, window_spec)
}

/// Match a window spec against the window ids in a `window.list` response.
///
/// Scoped to one session's windows: a prefix that would collide across the
/// whole daemon can still be unique inside the session the caller named.
pub fn resolve_window_id_by_id(
    windows: &[serde_json::Value],
    window_spec: &str,
) -> Result<(String, String), RpcClientError> {
    use shux_core::idref::{ParsedRef, RefKind, parse_ref};

    let not_found = || RpcClientError::Rpc {
        code: -32004,
        message: format!("window '{window_spec}' not found in session"),
        data: None,
    };

    let matches: Vec<&serde_json::Value> = match parse_ref(RefKind::Window, window_spec) {
        Ok(ParsedRef::Exact(uuid)) => {
            let canonical = uuid.hyphenated().to_string();
            windows
                .iter()
                .filter(|w| w.get("id").and_then(|v| v.as_str()) == Some(canonical.as_str()))
                .collect()
        }
        Ok(ParsedRef::Prefix(prefix)) => windows
            .iter()
            .filter(|w| {
                w.get("id").and_then(|v| v.as_str()).is_some_and(|id| {
                    id.replace('-', "")
                        .to_ascii_lowercase()
                        .starts_with(&prefix)
                })
            })
            .collect(),
        // Not id-shaped at all — it was a name that does not exist.
        Err(_) => return Err(not_found()),
    };

    match matches.len() {
        0 => Err(not_found()),
        1 => {
            let w = matches[0];
            let id = w.get("id").and_then(|v| v.as_str()).unwrap_or("?");
            let title = w.get("title").and_then(|v| v.as_str()).unwrap_or("?");
            Ok((id.to_string(), title.to_string()))
        }
        _ => {
            let mut ids: Vec<String> = matches
                .iter()
                .filter_map(|w| w.get("id").and_then(|v| v.as_str()))
                .map(|s| s.to_string())
                .collect();
            ids.sort();
            Err(ambiguous_ref_error("window", window_spec, &ids, None))
        }
    }
}

/// Resolve a pane-related window_id: either explicit window spec or session's active window.
pub async fn resolve_pane_window_id(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    window_spec: Option<&str>,
) -> Result<(String, String), RpcClientError> {
    let session_id = resolve_session_id(stream, session_name).await?;
    match window_spec {
        Some(spec) => {
            let (wid, _title) = resolve_window_id(stream, &session_id, spec).await?;
            Ok((session_id, wid))
        }
        None => {
            // Get active window from session. `include_scratch: true` so a
            // scratch session's pane (e.g. from `lens.run`'s `session_id`) is
            // still driveable without an explicit `--window` — the default
            // `session.list` visibility rule (LENS-R-041) is about listing,
            // not about whether a caller who already holds the id can act on
            // it (same "visibility != authorization" principle as the RPC).
            let result = rpc_call(
                stream,
                "session.list",
                serde_json::json!({ "include_scratch": true }),
            )
            .await?;
            let sessions = result
                .get("sessions")
                .and_then(|v| v.as_array())
                .or_else(|| result.as_array());
            if let Some(sessions) = sessions {
                for s in sessions {
                    if s.get("id").and_then(|v| v.as_str()) == Some(&session_id)
                        && let Some(aw) = s.get("active_window_id").and_then(|v| v.as_str())
                    {
                        return Ok((session_id, aw.to_string()));
                    }
                }
            }
            // The session is not in the list at all — say THAT. Blaming
            // window resolution points the reader at the wrong thing, and
            // it is the message a nonexistent session id produced.
            Err(RpcClientError::Rpc {
                code: -32004,
                message: format!("session '{session_name}' not found"),
                data: None,
            })
        }
    }
}

/// Confirm the pane reference names a pane inside the session, and return its
/// canonical full id.
///
/// The caller may hold a short id (issue #120), so the comparison is a
/// reference match rather than string equality — and the resolved id is
/// handed back so callers forward the canonical form to the daemon instead of
/// the fragment the user typed.
pub async fn validate_pane_belongs_to_session(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    pane_id: &str,
) -> Result<String, RpcClientError> {
    use shux_core::idref::{ParsedRef, RefKind, parse_ref};

    let parsed = parse_ref(RefKind::Pane, pane_id).map_err(|e| RpcClientError::Rpc {
        code: -32602,
        message: e.to_string(),
        // `detail` so this renders as the message alone rather than
        // "RPC error -32602: …" — same reason `ambiguous_ref_error` carries it.
        data: Some(serde_json::json!({ "detail": e.to_string() })),
    })?;
    let matches = |candidate: &str| -> bool {
        let bare = candidate.replace('-', "").to_ascii_lowercase();
        match &parsed {
            ParsedRef::Exact(uuid) => bare == uuid.simple().to_string(),
            ParsedRef::Prefix(prefix) => bare.starts_with(prefix),
        }
    };

    let session_id = resolve_session_id(stream, session_name).await?;
    let windows = rpc_call(
        stream,
        "window.list",
        serde_json::json!({"session_id": session_id}),
    )
    .await?;
    let Some(windows) = windows.as_array() else {
        return Err(RpcClientError::Rpc {
            code: -32004,
            message: "could not list session windows".to_string(),
            data: None,
        });
    };

    let mut hits: Vec<String> = Vec::new();
    for window in windows {
        let Some(window_id) = window.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        let panes = rpc_call(
            stream,
            "pane.list",
            serde_json::json!({"session_id": session_id, "window_id": window_id}),
        )
        .await?;
        if let Some(panes) = panes.as_array() {
            hits.extend(
                panes
                    .iter()
                    .filter_map(|p| p.get("id").and_then(|v| v.as_str()))
                    .filter(|id| matches(id))
                    .map(|id| id.to_string()),
            );
        }
    }
    hits.sort();

    match hits.len() {
        0 => Err(RpcClientError::Rpc {
            code: -32004,
            message: format!("pane {pane_id} does not belong to session {session_name}"),
            data: None,
        }),
        1 => Ok(hits.remove(0)),
        _ => Err(ambiguous_ref_error(
            "pane",
            pane_id,
            &hits,
            Some(session_name),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::test_support::*;
    use crate::cli::{args::*, session::*};

    /// codex P6 round-1 major 1, test (a): a session whose NAME is a
    /// UUID-shaped string (matching no real id) must remain addressable via
    /// `-s` and killable — the id-first resolution falls back to name lookup
    /// and targets the session's REAL id.
    #[tokio::test]
    async fn uuid_shaped_session_name_falls_back_to_name_lookup() {
        let real_id = "33333333-3333-4333-8333-333333333333";
        let uuid_shaped_name = "00000000-0000-4000-8000-000000000001";
        let list = serde_json::json!({
            "sessions": [{
                "id": real_id,
                "name": uuid_shaped_name,
                "active_window_id": "22222222-2222-4222-8222-222222222222",
                "created_at": 0
            }]
        });

        // Addressable via -s (resolve_session_id path).
        let (mut client, requests, task) = spawn_rpc_script(vec![list.clone()]);
        let resolved = resolve_session_id(&mut client, uuid_shaped_name)
            .await
            .unwrap();
        assert_eq!(resolved, real_id, "name fallback must yield the REAL id");
        let requests = finish_rpc_script(client, task, requests).await;
        assert_eq!(requests[0]["method"], "session.list");
        assert_eq!(requests[0]["params"]["include_scratch"], true);

        // Killable (handle_kill path) — the kill RPC targets the real id.
        let (mut client, requests, task) =
            spawn_rpc_script(vec![list, serde_json::json!({"killed": uuid_shaped_name})]);
        handle_kill(&mut client, uuid_shaped_name, None, OutputFormat::Json)
            .await
            .unwrap();
        let requests = finish_rpc_script(client, task, requests).await;
        assert_eq!(requests[1]["method"], "session.kill");
        assert_eq!(
            requests[1]["params"]["id"], real_id,
            "kill must target the session RESOLVED BY NAME, not the raw arg"
        );
        assert!(requests[1]["params"].get("name").is_none());
    }

    /// claude P6 round-1 extra: `Uuid::parse_str` ALSO accepts the 32-hex
    /// SIMPLE form — a session NAMED e.g. `deadbeef…` (32 hex chars) hits
    /// the same trap. Name fallback must cover it, AND a 32-hex arg that
    /// denotes a REAL session id (in canonical hyphenated form) must
    /// id-match through NORMALIZATION, not raw string equality.
    #[tokio::test]
    async fn simple_form_32hex_input_normalizes_and_falls_back() {
        // (i) session NAMED a 32-hex string, matching no real id → name
        // fallback resolves to its real id.
        let real_id = "33333333-3333-4333-8333-333333333333";
        let hex_name = "deadbeefdeadbeefdeadbeefdeadbeef"; // parses as a UUID
        let list = serde_json::json!({
            "sessions": [{ "id": real_id, "name": hex_name, "created_at": 0 }]
        });
        let (mut client, requests, task) = spawn_rpc_script(vec![list]);
        let resolved = resolve_session_id(&mut client, hex_name).await.unwrap();
        assert_eq!(
            resolved, real_id,
            "32-hex NAME must fall back to name lookup"
        );
        finish_rpc_script(client, task, requests).await;

        // (ii) 32-hex arg denoting a REAL session id → id-match via the
        // normalized hyphenated form; kill targets the canonical id.
        let hyphenated = "11111111-1111-4111-8111-111111111111";
        let simple = "11111111111141118111111111111111";
        let list = serde_json::json!({
            "sessions": [{ "id": hyphenated, "name": "dev", "created_at": 0 }]
        });
        let (mut client, requests, task) =
            spawn_rpc_script(vec![list, serde_json::json!({"killed": "dev"})]);
        handle_kill(&mut client, simple, None, OutputFormat::Json)
            .await
            .unwrap();
        let requests = finish_rpc_script(client, task, requests).await;
        assert_eq!(requests[1]["method"], "session.kill");
        assert_eq!(
            requests[1]["params"]["id"], hyphenated,
            "simple-form input must id-match through normalization and send the canonical id"
        );
    }

    /// codex P6 round-1 major 1, test (b): when the argument matches a REAL
    /// session id, the id wins — even when a DIFFERENT session is NAMED that
    /// same string (documented precedence; a warning is printed).
    #[tokio::test]
    async fn uuid_arg_matching_real_id_wins_over_name_match() {
        let arg = "11111111-1111-4111-8111-111111111111";
        let other_id = "44444444-4444-4444-8444-444444444444";
        let list = serde_json::json!({
            "sessions": [
                { "id": arg, "name": "dev", "created_at": 0 },
                // A different session NAMED the same UUID string — genuine
                // ambiguity; the id must win.
                { "id": other_id, "name": arg, "created_at": 0 }
            ]
        });
        let (mut client, requests, task) =
            spawn_rpc_script(vec![list, serde_json::json!({"killed": "dev"})]);
        handle_kill(&mut client, arg, None, OutputFormat::Json)
            .await
            .unwrap();
        let requests = finish_rpc_script(client, task, requests).await;
        assert_eq!(requests[1]["method"], "session.kill");
        assert_eq!(
            requests[1]["params"]["id"], arg,
            "id match must win over the name match (documented precedence)"
        );
    }

    /// codex P6 round-1 major 1, test (c): a bogus UUID matching neither an
    /// id nor a name is passed through as an id and surfaces the server's
    /// canonical not-found error cleanly.
    #[tokio::test]
    async fn bogus_uuid_neither_id_nor_name_errors_cleanly() {
        let bogus = "99999999-9999-4999-8999-999999999999";
        let list = serde_json::json!({
            "sessions": [{ "id": "11111111-1111-4111-8111-111111111111",
                           "name": "dev", "created_at": 0 }]
        });
        let not_found = serde_json::json!({
            "error": { "code": -32004, "message": "resource not found",
                       "data": { "resource": "session", "id": bogus } }
        });
        let (mut client, requests, task) = spawn_rpc_script(vec![list, not_found]);
        let err = handle_kill(&mut client, bogus, None, OutputFormat::Json)
            .await
            .expect_err("bogus UUID must surface the server's not-found");
        assert!(
            err.to_string().contains("not found"),
            "error must be the canonical not-found, got: {err}"
        );
        let requests = finish_rpc_script(client, task, requests).await;
        assert_eq!(requests[1]["method"], "session.kill");
        assert_eq!(requests[1]["params"]["id"], bogus);
    }

    /// The short id printed in `session list`'s last column resolves via `-s`.
    #[tokio::test]
    async fn short_session_id_resolves_after_the_name_lookup_misses() {
        let real = "abcd1234-1111-4111-8111-111111111111";
        let (mut client, requests, task) = spawn_rpc_script(vec![
            multi_session_list(&[(real, "work")]),
            multi_session_list(&[(real, "work")]),
        ]);
        let resolved = resolve_session_id(&mut client, "abcd1234").await.unwrap();
        assert_eq!(resolved, real);
        let requests = finish_rpc_script(client, task, requests).await;
        // Name lookup first (default visibility), then the prefix pass with
        // scratch included — visibility is not authorization.
        assert_eq!(requests[0]["method"], "session.list");
        assert!(requests[0]["params"].get("include_scratch").is_none());
        assert_eq!(requests[1]["params"]["include_scratch"], true);
    }

    /// An exact NAME always beats a partial id, even when the name is itself a
    /// valid hex prefix of a different session's id.
    #[tokio::test]
    async fn an_exact_name_beats_an_id_prefix() {
        let prefixed = "abcd1234-1111-4111-8111-111111111111";
        let named = "99999999-2222-4222-8222-222222222222";
        let (mut client, requests, task) = spawn_rpc_script(vec![multi_session_list(&[
            (prefixed, "other"),
            (named, "abcd1234"),
        ])]);
        let resolved = resolve_session_id(&mut client, "abcd1234").await.unwrap();
        assert_eq!(
            resolved, named,
            "the session literally NAMED abcd1234 must win"
        );
        // One call only: the name matched, so the prefix pass never ran.
        let requests = finish_rpc_script(client, task, requests).await;
        assert_eq!(requests.len(), 1);
    }

    /// Two sessions sharing a prefix must produce a named collision, not a
    /// coin flip.
    #[tokio::test]
    async fn an_ambiguous_session_prefix_is_refused_with_candidates() {
        let a = "abcd1111-1111-4111-8111-111111111111";
        let b = "abcd2222-2222-4222-8222-222222222222";
        let list = multi_session_list(&[(a, "one"), (b, "two")]);
        let (mut client, requests, task) = spawn_rpc_script(vec![list.clone(), list]);
        let err = resolve_session_id(&mut client, "abcd").await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("ambiguous"), "{msg}");
        assert!(msg.contains(a), "{msg}");
        assert!(msg.contains(b), "{msg}");
        finish_rpc_script(client, task, requests).await;
    }

    /// A name that does not exist and is not id-shaped keeps the old wording —
    /// telling someone their session name "is not hex" would be nonsense.
    #[tokio::test]
    async fn a_missing_name_still_reports_not_found() {
        // One response only: "typo" is not hex-shaped, so the prefix pass
        // never issues a second `session.list`.
        let list = multi_session_list(&[("abcd1234-1111-4111-8111-111111111111", "work")]);
        let (mut client, requests, task) = spawn_rpc_script(vec![list]);
        let err = resolve_session_id(&mut client, "typo").await.unwrap_err();
        assert!(
            err.to_string().contains("session 'typo' not found"),
            "{err}"
        );
        finish_rpc_script(client, task, requests).await;
    }

    /// A prefix below the four-character floor is not silently resolved.
    #[tokio::test]
    async fn a_two_character_session_ref_is_not_treated_as_a_prefix() {
        let list = multi_session_list(&[("abcd1234-1111-4111-8111-111111111111", "work")]);
        let (mut client, requests, task) = spawn_rpc_script(vec![list]);
        let err = resolve_session_id(&mut client, "ab").await.unwrap_err();
        assert!(err.to_string().contains("session 'ab' not found"), "{err}");
        finish_rpc_script(client, task, requests).await;
    }

    /// `--window` accepts an index, a title, a full UUID and a short id — in
    /// that precedence, so nothing that resolved before resolves differently.
    #[tokio::test]
    async fn window_specs_resolve_by_index_title_uuid_and_prefix() {
        let cases = [
            ("0", "aaaa1111-1111-4111-8111-111111111111"),
            ("1", "aaaa2222-2222-4222-8222-222222222222"),
            ("logs", "aaaa2222-2222-4222-8222-222222222222"),
            (
                "aaaa2222-2222-4222-8222-222222222222",
                "aaaa2222-2222-4222-8222-222222222222",
            ),
            ("AAAA2222", "aaaa2222-2222-4222-8222-222222222222"),
            ("aaaa2222-2222", "aaaa2222-2222-4222-8222-222222222222"),
        ];
        for (spec, expect) in cases {
            let (mut client, requests, task) = spawn_rpc_script(vec![two_window_list()]);
            let (id, _title) = resolve_window_id(&mut client, "sid", spec).await.unwrap();
            assert_eq!(id, expect, "spec {spec}");
            finish_rpc_script(client, task, requests).await;
        }
    }

    /// A window prefix shared by two windows in the session is refused.
    #[tokio::test]
    async fn an_ambiguous_window_prefix_is_refused() {
        let (mut client, requests, task) = spawn_rpc_script(vec![two_window_list()]);
        let err = resolve_window_id(&mut client, "sid", "aaaa")
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("ambiguous"), "{msg}");
        assert!(
            msg.contains("aaaa1111-1111-4111-8111-111111111111"),
            "{msg}"
        );
        finish_rpc_script(client, task, requests).await;
    }

    /// A window title that is not id-shaped and matches nothing stays a plain
    /// not-found.
    #[tokio::test]
    async fn an_unknown_window_name_is_not_found() {
        let (mut client, requests, task) = spawn_rpc_script(vec![two_window_list()]);
        let err = resolve_window_id(&mut client, "sid", "nope")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not found"), "{err}");
        finish_rpc_script(client, task, requests).await;
    }

    /// A numeric index that is out of range must fall through to the title and
    /// id passes rather than resolving to the wrong window.
    #[tokio::test]
    async fn an_out_of_range_index_falls_through_instead_of_wrapping() {
        let (mut client, requests, task) = spawn_rpc_script(vec![two_window_list()]);
        let err = resolve_window_id(&mut client, "sid", "9")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not found"), "{err}");
        finish_rpc_script(client, task, requests).await;
    }

    /// `validate_pane_belongs_to_session` hands back the CANONICAL id, so a
    /// caller that was given a short id forwards the full one to the daemon.
    #[tokio::test]
    async fn pane_membership_check_accepts_a_prefix_and_returns_the_full_id() {
        let sid = "11111111-1111-4111-8111-111111111111";
        let wid = "22222222-2222-4222-8222-222222222222";
        let pid = "33333333-3333-4333-8333-333333333333";
        let (mut client, requests, task) = spawn_rpc_script(vec![
            session_list_response(sid, wid),
            window_list_response(wid, pid),
            serde_json::json!([{"id": pid, "window_id": wid, "cwd": "/tmp",
                                "command": [], "title": "sh", "is_focused": true,
                                "is_zoomed": false, "version": 1}]),
        ]);
        let resolved = validate_pane_belongs_to_session(&mut client, "dev", "33333333")
            .await
            .unwrap();
        assert_eq!(resolved, pid);
        finish_rpc_script(client, task, requests).await;
    }

    /// A pane that belongs to a different session is still rejected — the
    /// prefix match must not widen the membership check.
    #[tokio::test]
    async fn pane_membership_check_still_rejects_a_foreign_pane() {
        let sid = "11111111-1111-4111-8111-111111111111";
        let wid = "22222222-2222-4222-8222-222222222222";
        let pid = "33333333-3333-4333-8333-333333333333";
        let (mut client, requests, task) = spawn_rpc_script(vec![
            session_list_response(sid, wid),
            window_list_response(wid, pid),
            serde_json::json!([{"id": pid, "window_id": wid, "cwd": "/tmp",
                                "command": [], "title": "sh", "is_focused": true,
                                "is_zoomed": false, "version": 1}]),
        ]);
        let err = validate_pane_belongs_to_session(&mut client, "dev", "44444444")
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("does not belong"),
            "the message names the mismatch instead of collapsing to a bare \
             \"resource not found\": {err}"
        );
        finish_rpc_script(client, task, requests).await;
    }

    /// Two panes in the session sharing a prefix collide rather than picking.
    #[tokio::test]
    async fn pane_membership_check_refuses_an_ambiguous_prefix() {
        let sid = "11111111-1111-4111-8111-111111111111";
        let wid = "22222222-2222-4222-8222-222222222222";
        let p1 = "33333333-1111-4111-8111-111111111111";
        let p2 = "33333333-2222-4222-8222-222222222222";
        let (mut client, requests, task) = spawn_rpc_script(vec![
            session_list_response(sid, wid),
            window_list_response(wid, p1),
            serde_json::json!([
                {"id": p1, "window_id": wid, "cwd": "/tmp", "command": [],
                 "title": "sh", "is_focused": true, "is_zoomed": false, "version": 1},
                {"id": p2, "window_id": wid, "cwd": "/tmp", "command": [],
                 "title": "sh", "is_focused": false, "is_zoomed": false, "version": 1},
            ]),
        ]);
        let err = validate_pane_belongs_to_session(&mut client, "dev", "33333333")
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("ambiguous"), "{msg}");
        assert!(msg.contains(p1) && msg.contains(p2), "{msg}");
        finish_rpc_script(client, task, requests).await;
    }
}
