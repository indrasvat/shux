//! `client.list` and `client.detach`: who is attached, and ending one of them.

use std::sync::Arc;

use shux_core::idref::{self, ParsedRef, RefKind};
use shux_rpc::{Policy, Sensitivity};
use tokio::sync::Mutex;

use crate::pane_io::PaneIoState;

pub(crate) fn register_client_methods(
    builder: shux_rpc::RouterBuilder,
    graph: shux_core::graph::GraphHandle,
    io_state: Arc<Mutex<PaneIoState>>,
) -> shux_rpc::RouterBuilder {
    let io_list = io_state.clone();
    let io_detach = io_state;
    builder
        .register_with_policy(
            "client.list",
            Policy::fixed(Sensitivity::Public),
            move |_params: Option<serde_json::Value>| {
                let io = io_list.clone();
                let gh = graph.clone();
                async move {
                    let clients = io.lock().await.clients.clone();
                    let snap = gh.snapshot();
                    let rows: Vec<serde_json::Value> = clients
                        .list()
                        .into_iter()
                        .map(|c| {
                            let attached_at = c
                                .attached_at
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_secs())
                                .unwrap_or(0);
                            serde_json::json!({
                                "id": c.id.0.to_string(),
                                "session_id": c.session_id.to_string(),
                                "session": snap.sessions.get(&c.session_id).map(|s| s.name.clone()),
                                "cols": c.cols,
                                "rows": c.rows,
                                "attached_at": attached_at,
                            })
                        })
                        .collect();
                    Ok(serde_json::json!({ "clients": rows }))
                }
            },
        )
        .register_with_policy(
            "client.detach",
            Policy::fixed(Sensitivity::Grantable),
            move |params: Option<serde_json::Value>| {
                let io = io_detach.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let raw = match params.get("id") {
                        Some(serde_json::Value::String(s)) => s,
                        Some(_) => {
                            return Err(shux_rpc::RpcError::invalid_params(
                                "'id' must be a string",
                            ));
                        }
                        None => return Err(shux_rpc::RpcError::invalid_params("missing 'id'")),
                    };
                    let not_found = || {
                        shux_rpc::RpcError::with_data(
                            shux_rpc::ErrorCode::NotFound,
                            serde_json::json!({
                                "resource": "client",
                                "id": raw.trim(),
                                "hint": "`shux client list` shows the attached clients",
                            }),
                        )
                    };
                    let parsed = idref::parse_ref(RefKind::Client, raw)
                        .map_err(|e| super::params::ref_error_to_rpc(e, "id"))?;
                    let clients = io.lock().await.clients.clone();
                    let target = match parsed {
                        ParsedRef::Exact(id) => id,
                        ParsedRef::Prefix(prefix) => idref::match_prefix(
                            RefKind::Client,
                            raw,
                            &prefix,
                            clients.list().into_iter().map(|c| c.id.0),
                        )
                        .map_err(|e| match e {
                            idref::RefError::NotFound { .. } => not_found(),
                            _ => super::params::ref_error_to_rpc(e, "id"),
                        })?,
                    };
                    if !clients.remove(target.into()) {
                        return Err(not_found());
                    }
                    Ok(serde_json::json!({ "detached": target.to_string() }))
                }
            },
        )
}

#[cfg(test)]
mod tests {
    use crate::clients::ClientInfo;
    use crate::rpc::test_harness::{RpcHarness, dispatch_err, dispatch_ok};
    use shux_core::event::ClientId;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn list_names_each_client_and_detach_takes_a_unique_prefix() {
        let h = RpcHarness::new();
        let (session_id, _, _) = h.seed_session("watched").await;
        let clients = h.io.lock().await.clients.clone();
        let info = |id: uuid::Uuid| ClientInfo {
            id: ClientId::from(id),
            session_id,
            cols: 100,
            rows: 30,
            attached_at: std::time::SystemTime::now(),
        };
        // Two ids sharing a prefix, so a short one is ambiguous.
        let a = uuid::Uuid::parse_str("aaaa1111-0000-4000-8000-000000000000").unwrap();
        let b = uuid::Uuid::parse_str("aaaa2222-0000-4000-8000-000000000000").unwrap();
        let (end_a, end_b) = (CancellationToken::new(), CancellationToken::new());
        let _ga = clients.register(info(a), end_a.clone());
        let _gb = clients.register(info(b), end_b.clone());

        let listed = dispatch_ok(&h.router, "client.list", serde_json::json!({})).await;
        let rows = listed["clients"].as_array().expect("clients array");
        assert_eq!(rows.len(), 2, "{listed}");
        assert!(
            rows.iter()
                .all(|r| r["session"] == "watched" && r["cols"] == 100)
        );

        let ambiguous = dispatch_err(
            &h.router,
            "client.detach",
            serde_json::json!({"id": "aaaa"}),
        )
        .await;
        assert!(
            ambiguous
                .data
                .as_ref()
                .and_then(|d| d["detail"].as_str())
                .is_some_and(|s| s.contains("is ambiguous")),
            "{ambiguous:?}"
        );
        let wrong_type =
            dispatch_err(&h.router, "client.detach", serde_json::json!({"id": 5})).await;
        assert!(
            format!("{wrong_type:?}").contains("must be a string"),
            "{wrong_type:?}"
        );
        let missing = dispatch_err(
            &h.router,
            "client.detach",
            serde_json::json!({"id": "bbbb"}),
        )
        .await;
        assert_eq!(
            missing.code,
            shux_rpc::ErrorCode::NotFound.code(),
            "{missing:?}"
        );
        assert!(
            !end_a.is_cancelled() && !end_b.is_cancelled(),
            "a failed detach ended a client"
        );

        let done = dispatch_ok(
            &h.router,
            "client.detach",
            serde_json::json!({"id": " AAAA1 "}),
        )
        .await;
        assert_eq!(done["detached"], a.to_string(), "ids match case-blind");
        assert!(end_a.is_cancelled());
        assert!(!end_b.is_cancelled(), "detaching one client ended another");

        // Gone from the list the moment detach returns, even while its
        // connection is still closing.
        let sessions = dispatch_ok(&h.router, "session.list", serde_json::json!({})).await;
        assert_eq!(sessions["sessions"][0]["attached"], 1);
        let again = dispatch_err(
            &h.router,
            "client.detach",
            serde_json::json!({"id": "aaaa1"}),
        )
        .await;
        assert_eq!(
            again.code,
            shux_rpc::ErrorCode::NotFound.code(),
            "{again:?}"
        );

        // An empty id is a prefix of every id; it names no one.
        let empty = dispatch_err(&h.router, "client.detach", serde_json::json!({"id": ""})).await;
        assert_eq!(
            empty.code,
            shux_rpc::ErrorCode::InvalidParams.code(),
            "{empty:?}"
        );
        assert!(!end_b.is_cancelled(), "an empty id detached a client");

        for raw in ["a", "zzzz"] {
            let err =
                dispatch_err(&h.router, "client.detach", serde_json::json!({"id": raw})).await;
            assert_eq!(err.code, shux_rpc::ErrorCode::InvalidParams.code());
            assert!(!end_b.is_cancelled());
        }
        let done = dispatch_ok(
            &h.router,
            "client.detach",
            serde_json::json!({"id": b.simple().to_string()}),
        )
        .await;
        assert_eq!(done["detached"], b.to_string());
        assert!(end_b.is_cancelled());
    }
}
