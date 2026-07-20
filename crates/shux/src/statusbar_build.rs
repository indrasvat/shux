//! Shared status-bar builder used by both the live attach renderer
//! and the `window.snapshot` / `session.snapshot` rasterizer.
//!
//! Keeping this in one place is load-bearing: a snapshot PNG should
//! show the exact same bar a live attached client would see. Earlier
//! we had two separate builders (one in attach.rs, one in main.rs's
//! snapshot path) which drifted: the snapshot path still rendered the
//! v0.22.0 hardcoded "◆ session  [i/n] window  HH:MM:SS" bar after the
//! attach path moved on to the post-council OOTB design.

use std::time::{Duration, Instant};

use crossterm::style::Color;
use shux_core::graph::SessionGraphSnapshot;
use shux_core::model::{PaneId, SessionId, WindowId};
use shux_core::theme::{Rgb, Theme};
use shux_ui::{StatusBar, StatusSegment};

use crate::onboarding::OnboardingState;
use crate::session_meta::SessionMeta;

/// How long a transient command-feedback label (`[pane split]`) stays
/// in the center zone before clearing.
pub const ACTION_FEEDBACK_DWELL: Duration = Duration::from_millis(1500);

/// Inputs the builder needs that don't come from the graph snapshot.
/// The attach renderer fills this from live state; the snapshot path
/// fills it with defaults that produce a clean "first attach" bar.
pub struct StatusBarCtx<'a> {
    pub session_id: SessionId,
    pub session_name: &'a str,
    pub active_window_id: WindowId,
    /// Active focused pane — used by the `[pane split]` etc. transient
    /// feedback overlay path. Currently only consulted indirectly via
    /// `last_action`; we keep the field so future per-pane signals
    /// (zoom indicator from the pane vs. window, dirty indicator) have
    /// a place to land without another signature churn.
    #[allow(dead_code)] // reserved for future per-pane signals (see field doc)
    pub active_pane_id: PaneId,
    pub session_meta: &'a SessionMeta,
    pub onboarding: &'a OnboardingState,
    pub daemon_uptime: Duration,
    pub nerd_fonts: bool,
    /// User-facing prefix label like `^Sp` or `^B`. Built from
    /// `config.keys.prefix` via `prefix_display`.
    pub prefix_label: &'a str,
    pub client_cols: u16,
    /// True when the user is currently in copy-mode. Drives the `Y `
    /// flag in the center zone.
    pub copy_mode_active: bool,
    /// Most-recent action label + when it fired. Transient overlay.
    /// `None` for the snapshot path (which has no input history).
    pub last_action: Option<(&'a str, Instant)>,
}

/// Human-readable label for an action's transient feedback overlay.
/// Returns None for actions whose effect is self-evident (focus /
/// resize / window-switch / redraw) — we don't want to flash
/// `[focused right]` on every directional nudge.
pub fn action_feedback_label(kind: shux_rpc::attach::ActionKind) -> Option<&'static str> {
    use shux_rpc::attach::ActionKind::*;
    Some(match kind {
        SplitVertical | SplitHorizontal | SplitSmart => "pane split",
        ToggleZoom => "zoom toggled",
        KillPane => "pane closed",
        NewWindow => "+ window",
        EnterCopyMode => "copy mode",
        _ => return None,
    })
}

/// Render a config-format prefix string (`ctrl-space`, `ctrl-b`,
/// `alt-w`) into the conventional short label (`^Sp`, `^B`, `M-W`).
pub fn prefix_display(prefix: &str) -> String {
    let lower = prefix.trim().to_lowercase();
    let (modifier, key) = match lower.split_once('-') {
        Some((m, k)) => (m, k),
        None => return prefix.to_string(),
    };
    let mod_short = match modifier {
        "ctrl" => "^",
        "alt" => "M-",
        "shift" => "S-",
        "super" | "cmd" => "Cmd-",
        m => return format!("{m}-{key}"),
    };
    let key_short: &str = match key {
        "space" => "Sp",
        "tab" => "Tab",
        "enter" | "return" => "Enter",
        k if k.len() == 1 => {
            return format!("{mod_short}{}", k.to_ascii_uppercase());
        }
        k => k,
    };
    format!("{mod_short}{key_short}")
}

/// Compact uptime: `up 2h17m`, `up 42m`, `up 8s`. Drops seconds once
/// we're above a minute so the renderer doesn't tick every frame.
pub fn format_uptime(d: Duration) -> String {
    let secs = d.as_secs();
    let hours = secs / 3600;
    let minutes = (secs % 3600) / 60;
    if hours > 0 {
        format!("up {hours}h{minutes:02}m")
    } else if minutes > 0 {
        format!("up {minutes}m")
    } else {
        format!("up {secs}s")
    }
}

/// The actual OOTB bar. See module-level docs for design rationale.
pub fn build(snap: &SessionGraphSnapshot, theme: &Theme, ctx: &StatusBarCtx<'_>) -> StatusBar {
    let to_color = |rgb: Rgb| Color::Rgb {
        r: rgb.r,
        g: rgb.g,
        b: rgb.b,
    };
    let mut bar = StatusBar::new();
    bar.bg = Some(to_color(theme.status_bg));

    let cols = ctx.client_cols;
    let show_branch = cols >= 80 && ctx.session_meta.git_branch.is_some();
    let show_pane_count = cols >= 80;
    let show_uptime = cols >= 120 && ctx.onboarding.prefix_discovered;
    let show_ssh = cols >= 100 && ctx.session_meta.over_ssh;

    let icon_session = if ctx.nerd_fonts { "\u{f489}" } else { "◆" };
    let icon_branch = if ctx.nerd_fonts { "\u{e0a0}" } else { "±" };
    let icon_window = if ctx.nerd_fonts { "\u{f489}" } else { "▶" };
    let icon_ssh = if ctx.nerd_fonts { "\u{f015}" } else { "@" };

    // LEFT — identity
    bar.left.push(StatusSegment::styled(
        format!(" {icon_session} {} ", ctx.session_name),
        to_color(theme.status_accent),
        true,
    ));
    if show_branch && let Some(branch) = ctx.session_meta.git_branch.as_ref() {
        bar.left.push(StatusSegment::styled(
            format!("{icon_branch} {branch} "),
            to_color(theme.status_branch),
            false,
        ));
    }
    if show_ssh {
        bar.left.push(StatusSegment::styled(
            format!("{icon_ssh} ssh "),
            to_color(theme.status_muted),
            false,
        ));
    }

    // CENTER — window nav + mode flags (+ transient overlay)
    if let Some(sess) = snap.sessions.get(&ctx.session_id) {
        let win_count = sess.windows.len();
        let active_idx = sess
            .windows
            .iter()
            .position(|w| *w == ctx.active_window_id)
            .unwrap_or(0);
        if let Some(win) = snap.windows.get(&ctx.active_window_id) {
            let title = if win.title.is_empty() {
                "shell".to_string()
            } else {
                win.title.clone()
            };

            let mut flags = String::new();
            if win.layout.is_zoomed() {
                flags.push_str("Z ");
            }
            if ctx.copy_mode_active {
                flags.push_str("Y ");
            }
            if !flags.is_empty() {
                bar.center.push(StatusSegment::styled(
                    flags,
                    to_color(theme.status_accent),
                    true,
                ));
            }

            let pane_count = win.layout.tree.pane_ids().len();
            let center_text = if show_pane_count {
                let pane_label = if pane_count == 1 { "pane" } else { "panes" };
                format!(
                    " {icon_window} #{}/{} {} · {} {pane_label} ",
                    active_idx + 1,
                    win_count,
                    title,
                    pane_count
                )
            } else {
                format!(
                    " {icon_window} #{}/{} {} ",
                    active_idx + 1,
                    win_count,
                    title
                )
            };
            bar.center.push(StatusSegment::styled(
                center_text,
                to_color(theme.status_fg),
                false,
            ));

            if let Some((label, at)) = ctx.last_action
                && at.elapsed() < ACTION_FEEDBACK_DWELL
            {
                bar.center.clear();
                bar.center.push(StatusSegment::styled(
                    format!(" [{}] ", label),
                    to_color(theme.status_accent),
                    true,
                ));
            }
        }
    }

    // RIGHT — onboarding hint OR uptime
    if !ctx.onboarding.prefix_discovered {
        let hint = if cols >= 80 {
            format!(
                " {} ?  help · {} d  detach ",
                ctx.prefix_label, ctx.prefix_label
            )
        } else {
            format!(" {} ?  help ", ctx.prefix_label)
        };
        bar.right.push(StatusSegment::styled(
            hint,
            to_color(theme.status_muted),
            false,
        ));
    } else if show_uptime {
        bar.right.push(StatusSegment::styled(
            format!(" {} ", format_uptime(ctx.daemon_uptime)),
            to_color(theme.status_muted),
            false,
        ));
    }

    bar
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_display_canonical_forms() {
        assert_eq!(prefix_display("ctrl-space"), "^Sp");
        assert_eq!(prefix_display("ctrl-b"), "^B");
        assert_eq!(prefix_display("alt-w"), "M-W");
        assert_eq!(prefix_display("super-tab"), "Cmd-Tab");
        // Unrecognized modifier falls through with the canonical form.
        assert_eq!(prefix_display("hyper-x"), "hyper-x");
    }

    #[test]
    fn format_uptime_tiers() {
        assert_eq!(format_uptime(Duration::from_secs(8)), "up 8s");
        assert_eq!(format_uptime(Duration::from_secs(60)), "up 1m");
        assert_eq!(format_uptime(Duration::from_secs(60 * 42)), "up 42m");
        assert_eq!(
            format_uptime(Duration::from_secs(3600 * 2 + 60 * 17)),
            "up 2h17m"
        );
    }

    #[test]
    fn action_label_is_terse_or_silent() {
        use shux_rpc::attach::ActionKind::*;
        assert_eq!(action_feedback_label(KillPane), Some("pane closed"));
        assert_eq!(action_feedback_label(ToggleZoom), Some("zoom toggled"));
        assert_eq!(action_feedback_label(SplitVertical), Some("pane split"));
        assert_eq!(action_feedback_label(FocusLeft), None);
        assert_eq!(action_feedback_label(ResizeLeft), None);
    }

    // ── width-tier integration tests ───────────────────────────────
    //
    // These exercise `build` end-to-end against a synthetic
    // SessionGraphSnapshot at four canonical widths (60 / 80 / 120 /
    // 200 cols). Each tier has a guarantee about what does / doesn't
    // appear: narrow drops branch + pane count, the hint never gets
    // pushed off, etc. If any of these regress the OOTB design
    // contract breaks silently — the rasterizer would happily render
    // a half-built bar.

    use shux_core::graph::SessionGraph;

    fn fixture() -> (
        SessionGraphSnapshot,
        SessionId,
        WindowId,
        PaneId,
        SessionMeta,
    ) {
        let (graph, state) = SessionGraph::new();
        let sid = graph
            .create_session("project".into(), std::path::PathBuf::from("/tmp"))
            .unwrap();
        let snap = state.load();
        let wid = snap.sessions[&sid].windows[0];
        let pid = snap.windows[&wid].active_pane;
        let meta = SessionMeta {
            git_branch: Some("main".into()),
            over_ssh: false,
        };
        (snap.as_ref().clone(), sid, wid, pid, meta)
    }

    fn ctx<'a>(
        sid: SessionId,
        name: &'a str,
        wid: WindowId,
        pid: PaneId,
        meta: &'a SessionMeta,
        onb: &'a OnboardingState,
        cols: u16,
    ) -> StatusBarCtx<'a> {
        StatusBarCtx {
            session_id: sid,
            session_name: name,
            active_window_id: wid,
            active_pane_id: pid,
            session_meta: meta,
            onboarding: onb,
            daemon_uptime: Duration::from_secs(0),
            nerd_fonts: false,
            prefix_label: "^Sp",
            client_cols: cols,
            copy_mode_active: false,
            last_action: None,
        }
    }

    fn flat(segs: &[shux_ui::StatusSegment]) -> String {
        segs.iter().map(|s| s.text.as_str()).collect()
    }

    #[test]
    fn narrow_width_drops_branch_and_pane_count_keeps_hint() {
        let (snap, sid, wid, pid, meta) = fixture();
        let onb = OnboardingState::default();
        let bar = build(
            &snap,
            &Theme::DEFAULT,
            &ctx(sid, "project", wid, pid, &meta, &onb, 60),
        );
        let left = flat(&bar.left);
        let center = flat(&bar.center);
        let right = flat(&bar.right);
        assert!(left.contains("project"), "session must always show: {left}");
        assert!(!left.contains("main"), "branch hidden at 60 cols: {left}");
        assert!(
            !center.contains("pane"),
            "no pane count at 60 cols: {center}"
        );
        assert!(right.contains("help"), "hint always shows: {right}");
    }

    #[test]
    fn wide_width_shows_full_identity_and_hint() {
        let (snap, sid, wid, pid, meta) = fixture();
        let onb = OnboardingState::default();
        let bar = build(
            &snap,
            &Theme::DEFAULT,
            &ctx(sid, "project", wid, pid, &meta, &onb, 120),
        );
        let left = flat(&bar.left);
        let center = flat(&bar.center);
        let right = flat(&bar.right);
        assert!(left.contains("project"));
        assert!(left.contains("main"), "branch at 120 cols: {left}");
        assert!(
            center.contains("1 pane"),
            "pane count at 120 cols: {center}"
        );
        assert!(right.contains("help"));
        assert!(right.contains("detach"), "detach hint at 120 cols: {right}");
    }

    #[test]
    fn dismissed_hint_shows_uptime_at_wide_widths() {
        let (snap, sid, wid, pid, meta) = fixture();
        let onb = OnboardingState {
            prefix_discovered: true,
            welcome_toast_seen: true,
        };
        let mut c = ctx(sid, "project", wid, pid, &meta, &onb, 120);
        c.daemon_uptime = Duration::from_secs(60 * 17 + 3600 * 2);
        let bar = build(&snap, &Theme::DEFAULT, &c);
        let right = flat(&bar.right);
        assert!(!right.contains("help"), "hint must be gone post-dismissal");
        assert!(right.contains("up 2h17m"), "uptime shown: {right}");
    }

    #[test]
    fn dismissed_hint_narrow_width_shows_nothing_on_right() {
        let (snap, sid, wid, pid, meta) = fixture();
        let onb = OnboardingState {
            prefix_discovered: true,
            welcome_toast_seen: true,
        };
        let bar = build(
            &snap,
            &Theme::DEFAULT,
            &ctx(sid, "project", wid, pid, &meta, &onb, 80),
        );
        // 80 cols, post-dismissal: hint gone, uptime gated at >=120 →
        // right zone is empty (calm, intentional).
        assert!(flat(&bar.right).is_empty());
    }
}
