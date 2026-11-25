//! The top-level parameters every method accepts.
//!
//! The router refuses any other key before the handler runs, and
//! `system.version` publishes this table as the daemon's capabilities. A new
//! parameter goes here, and in `wire_1.rs`, in the change that reads it; a
//! removed one is a [`shux_rpc::WIRE_GENERATION`] bump. A key set to `null`
//! reaches a handler only if 0.64.0 took it, so a newer parameter's `null`
//! always means absent. 0.64.0's surface is frozen in `legacy_0_64.rs`.

/// `pane_id`, `window_id` and `session_id`, which name a pane (or the active
/// one of a window or session) or scope events, then the method's own keys.
macro_rules! pane_target {
    ($($k:literal),* $(,)?) => { &["pane_id", "session_id", "window_id", $($k),*] };
}

pub(crate) const ACCEPTS: &[(&str, shux_rpc::Accepts)] = &[
    ("client.detach", &["id"]),
    ("client.list", &[]),
    (shux_rpc::RETIRE_IF_IDLE, &["client"]),
    (shux_rpc::UPGRADE, &["client", "when"]),
    ("events.history", pane_target!["count", "filter"]),
    (
        "events.watch",
        pane_target!["filter", "from_seq", "max_events", "timeout_ms"],
    ),
    (
        "lens.run",
        &[
            "argv",
            "cast",
            "cols",
            "cwd",
            "env",
            "env_clear",
            "max_runtime_ms",
            "post_exit_ttl_ms",
            "rows",
            "wait",
        ],
    ),
    ("pane.capture", pane_target!["lines"]),
    ("pane.checkpoint", pane_target![]),
    ("pane.command_cancel", &["command_id"]),
    ("pane.command_status", &["command_id"]),
    (
        "pane.diff_since",
        pane_target!["changed_row_text", "heat_png", "since_revision"],
    ),
    ("pane.focus", &["pane_id"]),
    (
        "pane.focus_direction",
        &["direction", "session_id", "window_id"],
    ),
    (
        "pane.glance",
        pane_target![
            "checkpoint",
            "include_cells",
            "include_cursor",
            "include_png",
            "masks"
        ],
    ),
    ("pane.kill", &["expected_version", "pane_id"]),
    ("pane.list", &["session_id", "window_id"]),
    (
        "pane.output.watch",
        &["from_seq", "limit", "pane_id", "timeout_ms"],
    ),
    (
        "pane.record.start",
        pane_target![
            "duration_ms",
            "format",
            "overwrite",
            "owned_by_connection",
            "path"
        ],
    ),
    ("pane.record.stop", &["recording_id"]),
    ("pane.record.wait", &["recording_id"]),
    (
        "pane.resize",
        pane_target!["delta", "direction", "expected_version"],
    ),
    (
        "pane.run_command",
        pane_target![
            "args",
            "async",
            "boundaries",
            "command",
            "from_pane",
            "timeout"
        ],
    ),
    ("pane.send_keys", pane_target!["data", "from_pane", "text"]),
    ("pane.set_size", pane_target!["cols", "rows", "unpin"]),
    ("pane.set_title", pane_target!["auto", "title"]),
    ("pane.snapshot", pane_target![]),
    (
        "pane.split",
        pane_target!["command", "cwd", "direction", "ratio"],
    ),
    ("pane.status", pane_target![]),
    (
        "pane.swap",
        &["expected_version", "pane_id", "target_pane_id"],
    ),
    (
        "pane.wait_command",
        pane_target!["after", "output", "timeout_ms"],
    ),
    ("pane.wait_exit", pane_target!["timeout_ms"]),
    (
        "pane.wait_for",
        pane_target!["absent", "lines", "poll_ms", "regex", "text", "timeout_ms"],
    ),
    (
        "pane.wait_settled",
        pane_target![
            "hold_ms",
            "masks",
            "quiet_ms",
            "stable_frames",
            "timeout_ms"
        ],
    ),
    (
        "pane.wait_status",
        pane_target!["after", "id", "match", "state", "timeout_ms"],
    ),
    ("pane.zoom", pane_target!["expected_version"]),
    ("plugin.audit", &["plugin", "tail"]),
    ("plugin.grant", &["method", "plugin", "subscribe", "target"]),
    ("plugin.grants", &["plugin"]),
    (
        "plugin.install",
        &[
            "args",
            "cwd",
            "expected_name",
            "expected_version",
            "path",
            "state_root",
            "watch",
        ],
    ),
    ("plugin.kill", &["name"]),
    ("plugin.list", &[]),
    ("plugin.reload", &["name"]),
    (
        "plugin.revoke",
        &["method", "plugin", "subscribe", "target"],
    ),
    ("session.create", &["command", "cwd", "name", "pane_title"]),
    ("session.ensure", &["command", "cwd", "name", "pane_title"]),
    ("session.export_template", &["id", "name"]),
    ("session.kill", &["expected_version", "id", "name"]),
    ("session.list", &["include_scratch"]),
    (
        "session.rename",
        &["expected_version", "id", "name", "new_name"],
    ),
    ("session.snapshot", &["cols", "rows", "session_id"]),
    ("share.confirm", &["id"]),
    (
        "share.create",
        &[
            "can_copy",
            "from_pane",
            "hold",
            "label",
            "qr",
            "session",
            "ttl_secs",
            "uses",
        ],
    ),
    ("share.kick", &["viewer"]),
    ("share.list", &[]),
    ("share.revoke", &["id"]),
    ("share.stop", &[]),
    ("state.apply", &["ops"]),
    ("system.health", &[]),
    ("system.version", &["client"]),
    ("window.create", &["command", "cwd", "name", "session_id"]),
    ("window.ensure", &["command", "cwd", "name", "session_id"]),
    ("window.focus", &["expected_version", "id"]),
    ("window.kill", &["expected_version", "id"]),
    ("window.list", &["session_id"]),
    ("window.rename", &["expected_version", "id", "name"]),
    ("window.reorder", &["expected_version", "id", "new_index"]),
    (
        "window.snapshot",
        &["cols", "rows", "session_id", "window_id"],
    ),
];
