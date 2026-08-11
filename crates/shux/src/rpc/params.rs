//! Parameter reading and id resolution shared by every handler.
//!
//! An id parameter that is present but of the wrong type is a caller
//! mistake, not a request to use the active entity — that distinction is
//! what [`optional_ref_param`] exists to keep.

use crate::rpc::convert::graph_error_to_rpc;

/// Extract optional `expected_version` from RPC params (PR 3b — optimistic
/// concurrency). Returns `Ok(None)` when the field is absent or null,
/// `Ok(Some(v))` when it's a valid non-negative integer, and an
/// `invalid_params` RpcError if it's the wrong type or out of range. The
/// daemon then plumbs the Option through to SessionGraph mutations, which
/// reject the request with `version_conflict` (-32002) if the entity has
/// moved since the client last read it.
pub(crate) fn parse_expected_version(
    params: &serde_json::Value,
) -> Result<Option<u64>, shux_rpc::RpcError> {
    match params.get("expected_version") {
        None => Ok(None),
        Some(v) if v.is_null() => Ok(None),
        Some(v) => v.as_u64().map(Some).ok_or_else(|| {
            shux_rpc::RpcError::invalid_params("'expected_version' must be a non-negative integer")
        }),
    }
}

/// Extract optional initial pane title from session.create/session.ensure params.
pub(crate) fn parse_initial_pane_title(
    params: &serde_json::Value,
) -> Result<Option<String>, shux_rpc::RpcError> {
    match params.get("pane_title") {
        None => Ok(None),
        Some(v) if v.is_null() => Ok(None),
        Some(v) => {
            let title = v.as_str().ok_or_else(|| {
                shux_rpc::RpcError::invalid_params("'pane_title' must be a string")
            })?;
            if title.trim().is_empty() {
                return Err(shux_rpc::RpcError::invalid_params(
                    "'pane_title' must not be empty",
                ));
            }
            Ok(Some(title.to_string()))
        }
    }
}

pub(crate) fn initial_pane_id_for_session(
    snap: &shux_core::graph::SessionGraphSnapshot,
    session_id: shux_core::model::SessionId,
) -> Result<shux_core::model::PaneId, shux_rpc::RpcError> {
    let session = snap
        .sessions
        .get(&session_id)
        .ok_or_else(|| shux_rpc::RpcError::internal("session vanished after create"))?;
    let window_id = session
        .windows
        .first()
        .ok_or_else(|| shux_rpc::RpcError::internal("created session has no windows"))?;
    let window = snap
        .windows
        .get(window_id)
        .ok_or_else(|| shux_rpc::RpcError::internal("initial window vanished after create"))?;

    window
        .layout
        .tree
        .pane_ids()
        .into_iter()
        .next()
        .ok_or_else(|| shux_rpc::RpcError::internal("initial window has no panes"))
}

pub(crate) async fn set_initial_pane_title(
    gh: &shux_core::graph::GraphHandle,
    session_id: shux_core::model::SessionId,
    title: Option<String>,
) -> Result<(), shux_rpc::RpcError> {
    let Some(title) = title else {
        return Ok(());
    };

    let pane_id = {
        let snap = gh.snapshot();
        initial_pane_id_for_session(&snap, session_id)?
    };

    gh.set_pane_title(pane_id, Some(title), None)
        .await
        .map_err(graph_error_to_rpc)
}

// ── Entity id references (issue #120) ────────────────────────────────────
//
// Every id parameter on the RPC surface goes through these. They accept the
// full UUID (unchanged, never looked up — see `shux_core::idref`) and, new
// here, the 8-character short form that every `shux` listing and success line
// prints. Without them, the id a caller can read is not an id they can send.

/// Map a resolution failure onto the wire, keeping the three outcomes
/// distinguishable: bad syntax is the caller's typo (`invalid_params`),
/// an unmatched prefix is a missing entity (`not_found`), and a collision is a
/// parameter that names too much (`invalid_params`, with the candidates).
///
/// `param` is the request field the reference arrived in — usually the kind's
/// own name, but `pane.swap` carries two pane references and a caller reading
/// "pane 'abcd' is ambiguous" needs to know which of the two to lengthen.
pub(crate) fn ref_error_to_rpc(err: shux_core::idref::RefError, param: &str) -> shux_rpc::RpcError {
    use shux_core::idref::RefError;
    match &err {
        RefError::Malformed { .. } => {
            shux_rpc::RpcError::invalid_params(&format!("{param}: {err}"))
        }
        RefError::NotFound { kind, input } => shux_rpc::RpcError::not_found(kind.as_str(), input),
        RefError::Ambiguous {
            kind,
            input,
            candidates,
            total,
        } => shux_rpc::RpcError::ambiguous_ref(kind.as_str(), input, candidates, *total),
    }
}

/// Resolve a pane reference string (full UUID or unambiguous id prefix).
pub(crate) fn resolve_pane_ref(
    gh: &shux_core::graph::GraphHandle,
    input: &str,
) -> Result<shux_core::model::PaneId, shux_rpc::RpcError> {
    resolve_pane_ref_named(gh, input, "pane_id")
}

/// [`resolve_pane_ref`] for a parameter that is not literally `pane_id`.
pub(crate) fn resolve_pane_ref_named(
    gh: &shux_core::graph::GraphHandle,
    input: &str,
    param: &str,
) -> Result<shux_core::model::PaneId, shux_rpc::RpcError> {
    gh.snapshot()
        .resolve_pane_ref(input)
        .map_err(|e| ref_error_to_rpc(e, param))
}

/// Resolve a window reference string (full UUID or unambiguous id prefix).
pub(crate) fn resolve_window_ref(
    gh: &shux_core::graph::GraphHandle,
    input: &str,
    param: &str,
) -> Result<shux_core::model::WindowId, shux_rpc::RpcError> {
    gh.snapshot()
        .resolve_window_ref(input)
        .map_err(|e| ref_error_to_rpc(e, param))
}

/// Resolve a session reference string (full UUID or unambiguous id prefix).
pub(crate) fn resolve_session_ref(
    gh: &shux_core::graph::GraphHandle,
    input: &str,
    param: &str,
) -> Result<shux_core::model::SessionId, shux_rpc::RpcError> {
    gh.snapshot()
        .resolve_session_ref(input)
        .map_err(|e| ref_error_to_rpc(e, param))
}

/// Read a required string parameter, or report which one is missing.
pub(crate) fn required_str<'a>(
    params: &'a serde_json::Value,
    key: &str,
) -> Result<&'a str, shux_rpc::RpcError> {
    match optional_ref_param(params, key)? {
        Some(s) => Ok(s),
        None => Err(shux_rpc::RpcError::invalid_params(&format!(
            "missing '{key}' parameter"
        ))),
    }
}

/// Read an OPTIONAL id parameter that must be a string when present.
///
/// The distinction matters: an ABSENT id means "use the active one", but a
/// PRESENT id of the wrong JSON type is a caller mistake. Treating the two the
/// same — which `params.get(k).and_then(|v| v.as_str())` does — silently
/// retargets the call at whatever happens to be active, so
/// `{"pane_id": 12345, "session_id": "..."}` used to zoom, resize or send
/// keystrokes to a pane the caller never named, and report success.
pub(crate) fn optional_ref_param<'a>(
    params: &'a serde_json::Value,
    key: &str,
) -> Result<Option<&'a str>, shux_rpc::RpcError> {
    match params.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(s)) => Ok(Some(s.as_str())),
        Some(other) => Err(shux_rpc::RpcError::invalid_params(&format!(
            "{key} must be a string id, got {}",
            json_type_name(other)
        ))),
    }
}

/// The JSON type name of a value, for parameter-type errors.
pub(crate) fn json_type_name(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "a boolean",
        serde_json::Value::Number(_) => "a number",
        serde_json::Value::String(_) => "a string",
        serde_json::Value::Array(_) => "an array",
        serde_json::Value::Object(_) => "an object",
    }
}

/// Resolve a pane_id from params: either explicit `pane_id` or active pane of resolved window.
pub(crate) fn resolve_pane_id_from_params(
    gh: &shux_core::graph::GraphHandle,
    params: &serde_json::Value,
) -> Result<shux_core::model::PaneId, shux_rpc::RpcError> {
    if let Some(pane_id_str) = optional_ref_param(params, "pane_id")? {
        return resolve_pane_ref(gh, pane_id_str);
    }

    // Fall back to active pane of the resolved window
    let window_id = resolve_window_id_from_params(gh, params)?;
    let snap = gh.snapshot();
    let window = snap
        .windows
        .get(&window_id)
        .ok_or_else(|| shux_rpc::RpcError::not_found("window", &window_id.to_string()))?;
    Ok(window.active_pane)
}

/// Resolve a window_id from params: either explicit `window_id` or active window of session.
pub(crate) fn resolve_window_id_from_params(
    gh: &shux_core::graph::GraphHandle,
    params: &serde_json::Value,
) -> Result<shux_core::model::WindowId, shux_rpc::RpcError> {
    if let Some(wid_str) = optional_ref_param(params, "window_id")? {
        return resolve_window_ref(gh, wid_str, "window_id");
    }

    // Resolve from session
    let session_id_str = optional_ref_param(params, "session_id")?.ok_or_else(|| {
        shux_rpc::RpcError::invalid_params(
            "missing 'pane_id' or 'window_id' or 'session_id' parameter",
        )
    })?;

    let session_id = resolve_session_ref(gh, session_id_str, "session_id")?;

    let snap = gh.snapshot();
    let session = snap
        .sessions
        .get(&session_id)
        .ok_or_else(|| shux_rpc::RpcError::not_found("session", session_id_str))?;

    Ok(session.active_window)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::test_harness::{RpcHarness, dispatch_ok};
    use shux_core::graph::{GraphHandle, SessionGraph, run_graph_loop};
    use shux_core::layout::Direction;
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn set_initial_pane_title_targets_original_pane_after_focus_changes() {
        let (graph, state) = SessionGraph::new();
        let (cmd_tx, cmd_rx) = mpsc::channel(32);
        let token = CancellationToken::new();
        let token_clone = token.clone();
        let handle = tokio::spawn(async move {
            run_graph_loop(graph, cmd_rx, token_clone).await;
        });
        let gh = GraphHandle::new(cmd_tx, state);

        let session_id = gh
            .create_session_with_command(
                "title-race".to_string(),
                std::path::PathBuf::from("/tmp"),
                vec!["codex".to_string(), "--yolo".to_string()],
            )
            .await
            .unwrap();

        let original_pane = {
            let snap = gh.snapshot();
            initial_pane_id_for_session(&snap, session_id).unwrap()
        };
        let new_active = gh
            .split_pane(original_pane, Direction::Vertical, 0.5)
            .await
            .unwrap();

        set_initial_pane_title(&gh, session_id, Some("aww-shux".to_string()))
            .await
            .unwrap();

        let snap = gh.snapshot();
        assert_eq!(
            snap.panes[&original_pane].manual_title.as_deref(),
            Some("aww-shux")
        );
        assert_eq!(snap.panes[&original_pane].title, "aww-shux");
        assert_eq!(snap.panes[&new_active].manual_title, None);

        token.cancel();
        handle.await.unwrap();
    }

    /// Issue #120 — the RPC layer's id parameters accept the short form that
    /// every listing prints, and keep the three failure modes distinguishable.
    #[tokio::test]
    async fn rpc_id_parameters_accept_short_ids_and_keep_errors_distinct() {
        let harness = RpcHarness::new();
        dispatch_ok(
            &harness.router,
            "state.apply",
            serde_json::json!({
                "ops": [{
                    "op": "create_session",
                    "name": "shortid",
                    "cwd": "/tmp",
                    "initial_command": ["true"],
                    "initial_window_title": "dev"
                }]
            }),
        )
        .await;
        let snap = harness.graph.snapshot();
        let session = snap.find_session_by_name("shortid").expect("session");
        let session_id = session.id;
        let window_id = session.active_window;
        let pane_id = snap.windows[&window_id].active_pane;
        drop(snap);

        let short = |id: &str| id[..8].to_string();

        // Every id parameter resolves its own short form to the same entity.
        assert_eq!(
            resolve_pane_id_from_params(
                &harness.graph,
                &serde_json::json!({"pane_id": short(&pane_id.to_string())})
            )
            .unwrap(),
            pane_id
        );
        assert_eq!(
            resolve_window_id_from_params(
                &harness.graph,
                &serde_json::json!({"window_id": short(&window_id.to_string())})
            )
            .unwrap(),
            window_id
        );
        assert_eq!(
            resolve_window_id_from_params(
                &harness.graph,
                &serde_json::json!({"session_id": short(&session_id.to_string())})
            )
            .unwrap(),
            window_id,
            "a short session id must still resolve to that session's active window"
        );

        // …and so does a partially-hyphenated paste, and a shouted one.
        assert_eq!(
            resolve_pane_id_from_params(
                &harness.graph,
                &serde_json::json!({"pane_id": pane_id.to_string()[..13].to_uppercase()})
            )
            .unwrap(),
            pane_id
        );

        // Malformed stays invalid_params.
        let malformed =
            resolve_pane_id_from_params(&harness.graph, &serde_json::json!({"pane_id": "zzzz"}))
                .unwrap_err();
        assert_eq!(malformed.code, shux_rpc::ErrorCode::InvalidParams.code());
        assert!(
            malformed
                .data
                .as_ref()
                .and_then(|d| d["detail"].as_str())
                .is_some_and(|d| d.starts_with("pane_id:")),
            "the detail must name the offending parameter: {:?}",
            malformed.data
        );

        // A well-formed prefix that matches nothing is not_found, not
        // invalid_params — an agent branches on that difference.
        let orphan = if pane_id.to_string().starts_with("dead") {
            "beef"
        } else {
            "dead"
        };
        let missing =
            resolve_pane_id_from_params(&harness.graph, &serde_json::json!({"pane_id": orphan}))
                .unwrap_err();
        assert_eq!(missing.code, shux_rpc::ErrorCode::NotFound.code());

        // An unknown FULL uuid keeps its pre-#120 behaviour: the resolver
        // hands it straight to the handler, which reports its own not-found.
        assert_eq!(
            resolve_pane_id_from_params(
                &harness.graph,
                &serde_json::json!({"pane_id": "00000000-0000-4000-8000-000000000001"})
            )
            .unwrap()
            .to_string(),
            "00000000-0000-4000-8000-000000000001"
        );

        harness.stop().await;
    }

    /// The collision path cannot be provoked with real v4 ids (it would take
    /// hundreds of live panes), so its wire mapping is pinned directly.
    #[test]
    fn an_ambiguous_reference_maps_to_invalid_params_with_candidates() {
        let err = ref_error_to_rpc(
            shux_core::idref::RefError::Ambiguous {
                kind: shux_core::idref::RefKind::Pane,
                input: "abcd".to_string(),
                candidates: vec![
                    "abcd1111-1111-4111-8111-111111111111".to_string(),
                    "abcd2222-2222-4222-8222-222222222222".to_string(),
                ],
                total: 2,
            },
            "pane_id",
        );
        assert_eq!(err.code, shux_rpc::ErrorCode::InvalidParams.code());
        let data = err.data.expect("ambiguity data");
        assert_eq!(data["resource"], "pane");
        assert_eq!(data["id"], "abcd");
        assert_eq!(data["total"], 2);
        assert_eq!(
            data["candidates"],
            serde_json::json!([
                "abcd1111-1111-4111-8111-111111111111",
                "abcd2222-2222-4222-8222-222222222222"
            ]),
            "candidates must be machine-readable, not only in the message"
        );
        assert!(
            data["hint"].as_str().is_some_and(|h| h.contains("more")),
            "the hint must say what to do: {data}"
        );
    }

    /// The two pane references `pane.swap` takes must be distinguishable in
    /// an error, or a caller cannot tell which one to lengthen.
    #[test]
    fn a_second_pane_parameter_is_named_in_its_own_error() {
        let err = ref_error_to_rpc(
            shux_core::idref::RefError::Malformed {
                kind: shux_core::idref::RefKind::Pane,
                input: "zz".to_string(),
                reason: shux_core::idref::MalformedReason::NotHex,
            },
            "target_pane_id",
        );
        let detail = err.data.expect("detail").to_string();
        assert!(detail.contains("target_pane_id"), "{detail}");
    }
}
