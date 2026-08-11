//! Error and entity → JSON-RPC conversions shared by every handler.

/// Map GraphError to appropriate RPC error codes.
pub(crate) fn graph_error_to_rpc(e: shux_core::graph::GraphError) -> shux_rpc::RpcError {
    use shux_core::graph::GraphError;
    match e {
        // `data.id` is an ID, not a sentence. Passing `e.to_string()` here
        // ("pane not found: <uuid>") made the rendered message read
        // `pane 'pane not found: <uuid>' not found`.
        GraphError::SessionNotFound(id) => {
            shux_rpc::RpcError::not_found("session", &id.to_string())
        }
        GraphError::WindowNotFound(id) => shux_rpc::RpcError::not_found("window", &id.to_string()),
        GraphError::PaneNotFound(id) => shux_rpc::RpcError::not_found("pane", &id.to_string()),
        GraphError::SessionNameExists(ref name) => {
            shux_rpc::RpcError::name_conflict("session", name)
        }
        GraphError::WindowNameConflict(ref name) => {
            shux_rpc::RpcError::name_conflict("window", name)
        }
        GraphError::EmptySessionName
        | GraphError::SessionNameTooLong(_)
        | GraphError::InvalidSessionName(_) => shux_rpc::RpcError::invalid_params(&e.to_string()),
        GraphError::EmptyWindowName
        | GraphError::WindowNameTooLong(_)
        | GraphError::WindowIndexOutOfRange { .. } => {
            shux_rpc::RpcError::invalid_params(&e.to_string())
        }
        GraphError::LastWindow | GraphError::LastPane => {
            shux_rpc::RpcError::invalid_params(&e.to_string())
        }
        GraphError::PaneSwapSelf | GraphError::PaneCrossWindow | GraphError::NoNeighbor(_) => {
            shux_rpc::RpcError::invalid_params(&e.to_string())
        }
        GraphError::LayoutError(_) => shux_rpc::RpcError::internal(&e.to_string()),
        GraphError::VersionConflict {
            resource,
            ref id,
            expected,
            actual,
        } => shux_rpc::RpcError::version_conflict(resource, id, expected, actual),
        GraphError::Shutdown => shux_rpc::RpcError::internal(&e.to_string()),
    }
}

/// Build session info JSON from a Session, including window/pane IDs.
pub(crate) fn session_to_json(
    s: &shux_core::model::Session,
    snap: &shux_core::graph::SessionGraphSnapshot,
) -> serde_json::Value {
    let window_count = s.windows.len();
    let active_window_id = s.active_window.to_string();

    // Find window_id and pane_id for the first window
    let first_window_id = s.windows.first().map(|w| w.to_string());
    let first_pane_id = s
        .windows
        .first()
        .and_then(|wid| snap.windows.get(wid).map(|w| w.active_pane.to_string()));

    serde_json::json!({
        "id": s.id.to_string(),
        "name": s.name,
        "windows": s.windows.iter().map(|w| w.to_string()).collect::<Vec<_>>(),
        "window_count": window_count,
        "active_window_id": active_window_id,
        "window_id": first_window_id,
        "pane_id": first_pane_id,
        "created_at": s.created_at.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
    })
}

/// Build window info JSON from a Window.
pub(crate) fn window_to_json(
    w: &shux_core::model::Window,
    index: usize,
    is_active: bool,
    snap: &shux_core::graph::SessionGraphSnapshot,
) -> serde_json::Value {
    let pane_count = snap.panes.values().filter(|p| p.window_id == w.id).count();
    serde_json::json!({
        "id": w.id.to_string(),
        "session_id": w.session_id.to_string(),
        "title": w.title,
        "pane_count": pane_count,
        "active_pane_id": w.active_pane.to_string(),
        "index": index,
        "is_active": is_active,
        "version": w.version,
    })
}

/// Build pane info JSON from a Pane.
pub(crate) fn pane_to_json(
    p: &shux_core::model::Pane,
    window: &shux_core::model::Window,
) -> serde_json::Value {
    let is_focused = window.active_pane == p.id;
    let is_zoomed = window.layout.is_zoomed()
        && window
            .layout
            .zoom
            .as_ref()
            .is_some_and(|z| z.zoomed_pane == p.id);
    serde_json::json!({
        "id": p.id.to_string(),
        "window_id": p.window_id.to_string(),
        "title": p.title,
        "manual_title": p.manual_title,
        "osc_title": p.osc_title,
        "auto_title": p.auto_title,
        "cwd": p.cwd.to_string_lossy(),
        "command": p.command,
        "exit_status": p.exit_status,
        "is_focused": is_focused,
        "is_zoomed": is_zoomed,
        "version": p.version,
    })
}

/// Serialize an `Event` for JSON-RPC transport. Includes the typed payload
/// AND meta (seq, timestamp, type) at the top level so consumers can route
/// without recursing into a nested envelope.
pub(crate) fn event_to_json(event: &shux_core::event::Event) -> serde_json::Value {
    event.to_wire_json()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pane_io::PaneIoState;
    use crate::rpc::params::{parse_expected_version, parse_initial_pane_title};
    use crate::snapshot::{parse_snapshot_dims, preview_for_log, resolve_grid_default_colors};

    #[test]
    fn daemon_utility_mappers_cover_error_preview_snapshot_and_color_edges() {
        use shux_core::graph::GraphError;

        assert_eq!(
            graph_error_to_rpc(GraphError::SessionNotFound(
                shux_core::model::SessionId::new()
            ))
            .code,
            shux_rpc::ErrorCode::NotFound.code()
        );
        assert_eq!(
            graph_error_to_rpc(GraphError::WindowNotFound(shux_core::model::WindowId::new())).code,
            shux_rpc::ErrorCode::NotFound.code()
        );
        assert_eq!(
            graph_error_to_rpc(GraphError::PaneNotFound(shux_core::model::PaneId::new())).code,
            shux_rpc::ErrorCode::NotFound.code()
        );
        assert_eq!(
            graph_error_to_rpc(GraphError::WindowNameConflict("logs".to_string())).code,
            shux_rpc::ErrorCode::NameConflict.code()
        );
        assert_eq!(
            graph_error_to_rpc(GraphError::InvalidSessionName("bad/name".to_string())).code,
            shux_rpc::ErrorCode::InvalidParams.code()
        );
        assert_eq!(
            graph_error_to_rpc(GraphError::EmptyWindowName).code,
            shux_rpc::ErrorCode::InvalidParams.code()
        );
        assert_eq!(
            graph_error_to_rpc(GraphError::LastPane).code,
            shux_rpc::ErrorCode::InvalidParams.code()
        );
        assert_eq!(
            graph_error_to_rpc(GraphError::LayoutError("split failed".to_string())).code,
            shux_rpc::ErrorCode::InternalError.code()
        );
        assert_eq!(
            graph_error_to_rpc(GraphError::Shutdown).code,
            shux_rpc::ErrorCode::InternalError.code()
        );
        assert_eq!(
            graph_error_to_rpc(GraphError::VersionConflict {
                resource: "pane",
                id: "p1".to_string(),
                expected: 1,
                actual: 2,
            })
            .code,
            shux_rpc::ErrorCode::VersionConflict.code()
        );

        assert_eq!(
            parse_snapshot_dims(&serde_json::json!({"cols": 80, "rows": 24})).unwrap(),
            (80, 24)
        );
        assert_eq!(
            parse_snapshot_dims(&serde_json::json!({})).unwrap(),
            (120, 36)
        );
        assert_eq!(
            parse_snapshot_dims(&serde_json::json!({"cols": 3, "rows": 24}))
                .unwrap_err()
                .code,
            shux_rpc::ErrorCode::InvalidParams.code()
        );
        assert_eq!(
            parse_expected_version(&serde_json::json!({})).unwrap(),
            None
        );
        assert_eq!(
            parse_expected_version(&serde_json::json!({"expected_version": null})).unwrap(),
            None
        );
        assert_eq!(
            parse_expected_version(&serde_json::json!({"expected_version": 7})).unwrap(),
            Some(7)
        );
        assert_eq!(
            parse_expected_version(&serde_json::json!({"expected_version": "old"}))
                .unwrap_err()
                .code,
            shux_rpc::ErrorCode::InvalidParams.code()
        );
        assert_eq!(
            parse_initial_pane_title(&serde_json::json!({"pane_title": "editor"})).unwrap(),
            Some("editor".to_string())
        );
        assert_eq!(
            parse_initial_pane_title(&serde_json::json!({"pane_title": ""}))
                .unwrap_err()
                .code,
            shux_rpc::ErrorCode::InvalidParams.code()
        );
        assert_eq!(preview_for_log("short", 20), "short");
        assert_eq!(preview_for_log("one\ntwo\nthree", 5), "three");

        let default_io = PaneIoState::default();
        assert!(default_io.writers.is_empty());

        let mut grid = shux_vt::Grid::new(1, 2, shux_vt::GridConfig::default());
        resolve_grid_default_colors(&mut grid, shux_vt::TerminalDefaultColors::default());
        grid.visible_row_mut(0)[0].style.fg = shux_vt::Color::Default;
        grid.visible_row_mut(0)[1].style.bg = shux_vt::Color::Default;
        resolve_grid_default_colors(
            &mut grid,
            shux_vt::TerminalDefaultColors {
                fg: Some([1, 2, 3]),
                bg: Some([4, 5, 6]),
                cursor: None,
            },
        );
        assert_eq!(
            grid.visible_row(0)[0].style.fg,
            shux_vt::Color::Rgb(1, 2, 3)
        );
        assert_eq!(
            grid.visible_row(0)[1].style.bg,
            shux_vt::Color::Rgb(4, 5, 6)
        );
    }
}
