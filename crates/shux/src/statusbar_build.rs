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

use crate::clients::{Seat, Sharing};
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
    /// Why the last action failed. Shown in place of the centre zone, in
    /// the error colour, until the caller clears it.
    pub action_error: Option<&'a str>,
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
        // A branch name comes from whatever repo the operator cloned, and
        // `git check-refname-format` only forbids ASCII controls — C1,
        // U+2028 and the bidi overrides all survive into a ref. The status
        // bar paints each char straight into a cell on the outer terminal,
        // so it goes through the shared title rule (issue #104).
        let branch = shux_core::model::sanitize_title(branch);
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

            if let Some(error) = ctx.action_error {
                bar.center.clear();
                bar.center.push(StatusSegment::styled(
                    format!(" {error} "),
                    to_color(theme.status_error),
                    true,
                ));
            } else if let Some((label, at)) = ctx.last_action
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

/// "◆ N waiting" at the head of the right zone, for `waiting` panes blocked
/// on the user. Appended after every other segment, script ones included, so
/// it is the first thing dropped when the bar is full rather than painting
/// over another segment.
pub fn append_waiting(
    bar: &mut StatusBar,
    waiting: usize,
    theme: &Theme,
    nerd_fonts: bool,
    cols: u16,
) {
    if waiting == 0 {
        return;
    }
    let (unicode, nerd) = crate::pane_chrome::glyphs(shux_program_status::State::Blocked);
    let glyph = if nerd_fonts { nerd } else { unicode };
    let Rgb { r, g, b } = theme.status_attention;
    let segment = StatusSegment::styled(
        format!("{glyph} {waiting} waiting "),
        Color::Rgb { r, g, b },
        true,
    );
    let width = StatusBar::segments_width;
    let (lw, cw, rw) = (width(&bar.left), width(&bar.center), width(&bar.right));
    // StatusBar::render_row keeps a one-column gap either side of the centre.
    let gaps = if cw > 0 { 2 } else { 1 };
    if lw + cw + rw + gaps + width(std::slice::from_ref(&segment)) <= usize::from(cols) {
        bar.right.insert(0, segment);
    }
}

/// "◉ N attached" at the head of the right zone while more than one client
/// shows this session, so nobody is watched without seeing it. It outranks
/// every segment but the session name, and yields only to a passing notice:
/// on a full bar the others give way.
pub fn append_watchers(
    bar: &mut StatusBar,
    attached: usize,
    theme: &Theme,
    cols: u16,
    has_notice: bool,
) {
    if attached < 2 {
        return;
    }
    let Rgb { r, g, b } = theme.status_accent;
    let segment = StatusSegment::styled(
        format!("◉ {attached} attached "),
        Color::Rgb { r, g, b },
        true,
    );
    insert_at_head(bar, segment, cols, has_notice);
}

/// The focused pane's size label at the head of the right zone, when no
/// border shows it. It sheds words to fit the room the bar leaves and is
/// dropped whole rather than cut: `pinned W×H` → `W×H`, `⤡ W×H` → `⤡`.
pub fn append_viewing(
    bar: &mut StatusBar,
    label: Option<&shux_ui::viewport::SizeLabel>,
    theme: &Theme,
    cols: u16,
) {
    let Some(size) = label else {
        return;
    };
    let Rgb { r, g, b } = theme.status_attention;
    let width = StatusBar::segments_width;
    let (lw, cw, rw) = (width(&bar.left), width(&bar.center), width(&bar.right));
    let gaps = if cw > 0 { 2 } else { 1 };
    let room = usize::from(cols).saturating_sub(lw + cw + rw + gaps);
    let fits = size
        .ladder()
        .into_iter()
        .map(|t| StatusSegment::styled(format!("{t} "), Color::Rgb { r, g, b }, false))
        .find(|s| width(std::slice::from_ref(s)) <= room);
    if let Some(segment) = fits {
        bar.right.insert(0, segment);
    }
}

/// Marks the segments of a sharing badge: no fitting pass drops one.
const BADGE: usize = usize::MAX;

/// A live client's sharing badge: its words longest first, down to a letter
/// glued to the mark, the colour and whether it warns that another client's
/// keys reach the session.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Badge {
    rungs: Vec<String>,
    /// The first rung without the optional peer counts.
    core: usize,
    warn: bool,
}

impl Badge {
    /// None for a writer alone with its session.
    fn of(sharing: Sharing) -> Option<Self> {
        let Sharing {
            seat,
            writers,
            viewers,
        } = sharing;
        let watching = (viewers > 0).then(|| format!(" · {viewers} watching"));
        let typing = format!("{writers} can type");
        let (rungs, core, warn): (Vec<String>, usize, bool) = match seat {
            Seat::Viewer => (
                vec!["viewing · read-only".into(), "read-only".into(), "r".into()],
                0,
                false,
            ),
            Seat::Host if writers > 0 => {
                let core = format!("shared · {typing}");
                let mut rungs: Vec<String> =
                    watching.iter().map(|w| format!("{core}{w}")).collect();
                let counted = rungs.len();
                rungs.extend([core, typing, "!".into()]);
                (rungs, counted, true)
            }
            Seat::Host if viewers > 0 => (
                vec![
                    format!("shared · {viewers} watching"),
                    "shared".into(),
                    String::new(),
                ],
                1,
                false,
            ),
            Seat::Host => return None,
            Seat::Writer => {
                let core = "writable · your keys go to this session".to_string();
                let counts = format!(" · {typing}{}", watching.unwrap_or_default());
                (
                    vec![
                        format!("{core}{counts}"),
                        core,
                        "writable".into(),
                        "!".into(),
                    ],
                    1,
                    true,
                )
            }
        };
        Some(Self { rungs, core, warn })
    }

    fn unit(&self, rung: usize, theme: &Theme, mark: &str) -> Vec<StatusSegment> {
        let Rgb { r, g, b } = if self.warn {
            theme.status_error
        } else {
            theme.status_shared
        };
        let color = Color::Rgb { r, g, b };
        // A one-letter rung is glued to the mark, so the narrowest badge
        // still says what it is: `◉!` another client can type, `◉r` read-only.
        let words = match self.rungs[rung].as_str() {
            "" => " ".to_string(),
            w if w.chars().count() == 1 => format!("{w} "),
            w => format!(" {w} "),
        };
        [mark.to_string(), words]
            .into_iter()
            .map(|text| StatusSegment {
                group: BADGE,
                ..StatusSegment::styled(text, color, self.warn)
            })
            .collect()
    }
}

/// The sharing badge at the head of the right zone. It is never cut: its peer
/// counts go first, then the bar's other segments, then its words down to the
/// mark and one letter (`◉!` for a warning, `◉r` read-only), then the session
/// name. `mark` is `◉`, or `*` where colour cannot carry it.
pub fn append_sharing(
    bar: &mut StatusBar,
    sharing: Option<Sharing>,
    theme: &Theme,
    cols: u16,
    mark: &str,
    has_notice: bool,
) {
    let Some(badge) = sharing.and_then(Badge::of) else {
        return;
    };
    let width = StatusBar::segments_width;
    let cols = usize::from(cols);
    let fits = |bar: &StatusBar, unit: &[StatusSegment]| {
        let (lw, cw) = (width(&bar.left), width(&bar.center));
        lw + cw + width(&bar.right) + width(unit) + usize::from(lw > 0) + usize::from(cw > 0)
            <= cols
    };
    let put = |bar: &mut StatusBar, unit: Vec<StatusSegment>| {
        bar.right.splice(0..0, unit);
    };
    let full = badge.unit(0, theme, mark);
    if fits(bar, &full) {
        return put(bar, full);
    }
    let core = badge.unit(badge.core, theme, mark);
    loop {
        if fits(bar, &core) {
            return put(bar, core);
        }
        if !bar.right.is_empty() {
            pop_unit(&mut bar.right);
        } else if bar.left.len() > 1 {
            pop_unit(&mut bar.left);
        } else if !bar.center.is_empty() && !has_notice {
            bar.center.clear();
        } else {
            break;
        }
    }
    for rung in badge.core + 1..badge.rungs.len() {
        let unit = badge.unit(rung, theme, mark);
        if fits(bar, &unit) {
            return put(bar, unit);
        }
    }
    // Only the name is left: it gives up its tail, then itself.
    let unit = badge.unit(badge.rungs.len() - 1, theme, mark);
    let room = cols.saturating_sub(width(&unit) + 1 + width(&bar.center));
    if let Some(name) = bar.left.first_mut()
        && let Some(text) = shortened(&name.text, room)
    {
        name.text = text;
    } else {
        bar.left.clear();
    }
    put(bar, unit);
}

/// The status row without colour, for a client whose `NO_COLOR` is set.
pub fn monochrome(bar: &mut StatusBar) {
    bar.bg = None;
    for s in bar
        .left
        .iter_mut()
        .chain(&mut bar.center)
        .chain(&mut bar.right)
    {
        s.fg = None;
        s.bg = None;
    }
}

/// Put `segment` first on the right, and make the bar fit around it.
fn insert_at_head(bar: &mut StatusBar, segment: StatusSegment, cols: u16, has_notice: bool) {
    bar.right.insert(0, segment);
    let width = StatusBar::segments_width;
    let gaps = |bar: &StatusBar| {
        usize::from(width(&bar.left) > 0)
            + usize::from(width(&bar.center) > 0)
            + usize::from(width(&bar.right) > 0)
            - 1
    };
    while width(&bar.left) + width(&bar.center) + width(&bar.right) + gaps(bar) > usize::from(cols)
    {
        if bar.right.len() > 1 {
            pop_unit(&mut bar.right);
        } else if bar.left.len() > 1 {
            pop_unit(&mut bar.left);
        } else if has_notice {
            return;
        } else if !bar.center.is_empty() {
            bar.center.clear();
        } else {
            // Only the session name is left: it gives up its tail, not the count.
            let room = usize::from(cols).saturating_sub(width(&bar.right) + 1);
            if let Some(name) = bar.left.first_mut()
                && let Some(text) = shortened(&name.text, room)
            {
                name.text = text;
            } else {
                bar.left.clear();
            }
            return;
        }
    }
}

/// Drop the zone's last segment, and with it every other run its script printed.
fn pop_unit(zone: &mut Vec<StatusSegment>) {
    if let Some(last) = zone.pop()
        && last.group != 0
    {
        while zone.last().is_some_and(|s| s.group == last.group) {
            zone.pop();
        }
    }
}

/// Fit `seg` in `max` columns, ending in "…".
fn shortened(text: &str, max: usize) -> Option<String> {
    let mut kept = String::new();
    let mut used = 0;
    for (cell, w) in shux_ui::text_cells(text) {
        if used + usize::from(w) + 1 > max {
            break;
        }
        used += usize::from(w);
        kept.push_str(&cell);
    }
    let kept = kept.trim_end();
    (!kept.is_empty()).then(|| format!("{kept}…"))
}

/// Clear segments, innermost first, right zone before left, until a transient
/// notice in the centre fits whole: it is shown for a moment and says
/// something the user has to read, so it outranks both zones for that moment.
pub fn make_room_for_notice(bar: &mut StatusBar, cols: u16) {
    let width = StatusBar::segments_width;
    // StatusBar::render_row keeps a one-column gap beside each zone that has
    // something in it.
    let gaps =
        |bar: &StatusBar| usize::from(width(&bar.left) > 0) + usize::from(width(&bar.right) > 0);
    while width(&bar.left) + width(&bar.center) + width(&bar.right) + gaps(bar) > usize::from(cols)
    {
        // The sharing badge leads the right zone and stays: the notice is cut
        // before it is.
        if let Some(at) = bar.right.iter().position(|s| s.group != BADGE) {
            let first = bar.right.remove(at);
            if first.group != 0 {
                while bar.right.get(at).is_some_and(|s| s.group == first.group) {
                    bar.right.remove(at);
                }
            }
        } else if bar.left.is_empty() {
            return;
        } else {
            pop_unit(&mut bar.left);
        }
    }
}

#[cfg(test)]
mod tests {
    fn bar(left: &str, center: &str, right: &str) -> StatusBar {
        let mut b = StatusBar::new();
        for (zone, text) in [
            (&mut b.left, left),
            (&mut b.center, center),
            (&mut b.right, right),
        ] {
            if !text.is_empty() {
                zone.push(StatusSegment::plain(text));
            }
        }
        b
    }

    #[test]
    fn waiting_leads_the_right_zone_when_it_fits() {
        let mut b = bar("left", "", "right");
        append_waiting(&mut b, 2, &Theme::DEFAULT, false, 40);
        assert_eq!(b.right[0].text, "◆ 2 waiting ");
        assert_eq!(b.right[1].text, "right");
    }

    #[test]
    fn waiting_never_paints_over_a_segment_with_combining_marks() {
        let accented = format!("e{}", "\u{301}".repeat(10));
        let mut b = bar(&accented, "", "");
        append_waiting(&mut b, 1, &Theme::DEFAULT, false, 20);
        let row = b.render_row(20);
        assert_eq!(row[0].display_text(), accented, "the segment's cell");
        let text: String = row.iter().map(|c| c.ch).collect();
        assert!(text.ends_with("◆ 1 waiting "), "{text}");
        assert_eq!(text.chars().nth(1), Some(' '), "{text}");
    }

    #[test]
    fn waiting_is_dropped_rather_than_overpaint_a_full_bar() {
        // left 4 + centre 6 + right 5 + two gaps = 17; the segment is 12.
        let full = bar("left", "centre", "right");
        for (cols, fits) in [(29, true), (28, false)] {
            let mut b = full.clone();
            append_waiting(&mut b, 1, &Theme::DEFAULT, false, cols);
            assert_eq!(b.right.len() == 2, fits, "{cols} columns");
        }
        let mut none = full.clone();
        append_waiting(&mut none, 0, &Theme::DEFAULT, false, 200);
        assert_eq!(none.right.len(), 1, "nothing waiting, no segment");
    }

    #[test]
    fn a_second_client_is_shown_and_a_lone_one_is_not() {
        let mut alone = bar("left", "", "right");
        append_watchers(&mut alone, 1, &Theme::DEFAULT, 80, false);
        assert_eq!(alone.right.len(), 1, "a lone client sees a watcher count");
        let mut watched = bar("left", "", "right");
        append_watchers(&mut watched, 3, &Theme::DEFAULT, 80, false);
        assert_eq!(watched.right[0].text, "◉ 3 attached ");
    }

    #[test]
    fn watcher_count_yields_only_when_a_notice_needs_its_space() {
        let notice = format!(" [{}] ", "x".repeat(59));
        for (cols, show_count) in [(80, false), (100, true)] {
            let mut b = bar(" ◆ project ", &notice, "up 42m");
            append_watchers(&mut b, 2, &Theme::DEFAULT, cols, true);
            make_room_for_notice(&mut b, cols);
            let text: String = b.render_row(cols).iter().map(|c| c.ch).collect();
            assert!(text.contains(&notice), "{text:?}");
            assert_eq!(text.contains("◉ 2 attached"), show_count, "{text:?}");
        }
    }

    #[test]
    fn shortening_keeps_a_visible_prefix_and_trims_its_padding() {
        assert_eq!(shortened(" ◆ demo ", 2), None);
        assert_eq!(shortened(" ◆ demo ", 4).as_deref(), Some(" ◆…"));
        assert_eq!(shortened("日本", 2), None);
    }

    #[test]
    fn a_full_bar_gives_way_to_the_watcher_count() {
        // A user's script segments fill the bar: 4 + 6 + 30 + two gaps = 42.
        let mut b = bar("left", "centre", &"x".repeat(30));
        append_watchers(&mut b, 2, &Theme::DEFAULT, 42, false);
        let text: String = b.render_row(42).iter().map(|c| c.ch).collect();
        assert!(
            text.contains("◉ 2 attached"),
            "being watched was hidden: {text:?}"
        );
        assert!(
            text.starts_with("left"),
            "the session name was cleared: {text:?}"
        );
    }

    #[test]
    fn a_long_session_name_on_a_narrow_bar_still_shows_the_count() {
        let mut b = bar(&"日本".repeat(14), "", "r");
        append_watchers(&mut b, 9, &Theme::DEFAULT, 30, false);
        let text: String = b.render_row(30).iter().map(|c| c.ch).collect();
        assert!(text.contains("◉ 9 attached"), "the count was cut: {text:?}");
        assert!(text.starts_with('日') && text.contains('…'), "{text:?}");
    }

    #[test]
    fn a_script_segment_gives_way_whole_not_word_by_word() {
        let mut b = bar("left", "centre", "");
        for (text, group) in [("up 3h ", 0), ("via ", 7), ("v1.97.1 ", 7)] {
            b.right.push(StatusSegment {
                group,
                ..StatusSegment::plain(text)
            });
        }
        // Room for the count and "up 3h", not for the script's two runs.
        append_watchers(&mut b, 2, &Theme::DEFAULT, 35, false);
        let text: String = b.render_row(35).iter().map(|c| c.ch).collect();
        assert!(
            text.contains("◉ 2 attached") && text.contains("up 3h"),
            "{text:?}"
        );
        assert!(
            !text.contains("via"),
            "half a script segment was left: {text:?}"
        );
    }

    use super::*;

    /// The focused pane's size label sheds words to fit, and is dropped whole
    /// rather than cut mid-word.
    #[test]
    fn the_viewing_segments_follow_a_width_ladder() {
        use shux_ui::viewport::SizeLabel;
        let pinned = SizeLabel {
            cols: 60,
            rows: 8,
            pinned: true,
        };
        let cropped = SizeLabel {
            cols: 118,
            rows: 37,
            pinned: false,
        };
        let right = |label: &SizeLabel, cols: u16| {
            let mut b = StatusBar::new();
            b.left.push(StatusSegment::plain("s "));
            append_viewing(&mut b, Some(label), &Theme::DEFAULT, cols);
            b.right.iter().map(|s| s.text.as_str()).collect::<String>()
        };
        for (label, cols, want) in [
            (&pinned, 40, "pinned 60×8 "),
            (&pinned, 12, "60×8 "),
            (&pinned, 4, ""),
            (&cropped, 40, "⤡ 118×37 "),
            (&cropped, 8, "⤡ "),
        ] {
            assert_eq!(right(label, cols), want, "{cols} cols");
        }
    }

    fn sharing(seat: Seat, writers: usize, viewers: usize) -> Sharing {
        Sharing {
            seat,
            writers,
            viewers,
        }
    }

    /// The design's table, at full width: what each seat says, and whether
    /// it warns that another client's keys reach the session.
    #[test]
    fn each_seat_gets_its_badge() {
        let full = |s: Sharing| {
            let badge = Badge::of(s)?;
            Some((badge.rungs[0].clone(), badge.warn))
        };
        assert_eq!(full(sharing(Seat::Host, 0, 0)), None, "solo");
        for (s, words, warn) in [
            (sharing(Seat::Host, 0, 2), "shared · 2 watching", false),
            (sharing(Seat::Host, 1, 0), "shared · 1 can type", true),
            (
                sharing(Seat::Host, 1, 2),
                "shared · 1 can type · 2 watching",
                true,
            ),
            (sharing(Seat::Viewer, 1, 0), "viewing · read-only", false),
            (sharing(Seat::Viewer, 0, 0), "viewing · read-only", false),
            (
                sharing(Seat::Writer, 1, 0),
                "writable · your keys go to this session · 1 can type",
                true,
            ),
            (
                sharing(Seat::Writer, 1, 3),
                "writable · your keys go to this session · 1 can type · 3 watching",
                true,
            ),
        ] {
            assert_eq!(full(s), Some((words.to_string(), warn)), "{s:?}");
        }
    }

    /// A bar as a live client builds it, with every kind of segment the badge
    /// competes with.
    fn busy_bar() -> StatusBar {
        let mut b = bar("◆ my-session ", " #1/2 shell ", " up 3h ");
        b.left.push(StatusSegment::plain("main "));
        let label = shux_ui::viewport::SizeLabel {
            cols: 60,
            rows: 8,
            pinned: true,
        };
        append_viewing(&mut b, Some(&label), &Theme::DEFAULT, 200);
        b
    }

    fn badged(s: Sharing, cols: u16, notice: bool) -> (String, String, StatusBar) {
        let mut b = busy_bar();
        if notice {
            b.center = vec![StatusSegment::plain(" [pane split] ")];
        }
        append_sharing(&mut b, Some(s), &Theme::DEFAULT, cols, "◉", notice);
        if notice {
            make_room_for_notice(&mut b, cols);
        }
        let badge: String = b
            .right
            .iter()
            .filter(|s| s.group == BADGE)
            .map(|s| s.text.as_str())
            .collect();
        let row: String = b.render_row(cols).iter().map(|c| c.ch).collect();
        (badge, row, b)
    }

    /// Down to two columns the badge is drawn whole: counts go first, then
    /// the other segments, then its words, then the session name; a warning
    /// ends on `!`, which survives monochrome.
    #[test]
    fn the_badge_is_never_cut_down_to_two_columns() {
        for s in [
            sharing(Seat::Host, 1, 2),
            sharing(Seat::Host, 0, 2),
            sharing(Seat::Viewer, 2, 0),
            sharing(Seat::Writer, 1, 1),
        ] {
            let rungs = Badge::of(s).unwrap().rungs;
            let mut last = 0;
            for cols in (2..=140u16).rev() {
                for notice in [false, true] {
                    let (badge, row, _) = badged(s, cols, notice);
                    assert!(
                        row.contains(badge.trim_end()),
                        "{s:?} at {cols}: {row:?} cut {badge:?}"
                    );
                    assert!(badge.starts_with('◉'), "{s:?} at {cols}: {badge:?}");
                    let rung = rungs
                        .iter()
                        .position(|w| {
                            badge.trim_end() == format!("◉ {w}").trim_end()
                                || badge.trim_end() == format!("◉{w}")
                        })
                        .unwrap_or_else(|| panic!("{s:?} at {cols}: {badge:?} is no rung"));
                    if !notice {
                        assert!(rung >= last, "{s:?} at {cols}: longer badge in less room");
                        last = rung;
                    }
                }
            }
            // Down to two columns the mark keeps a letter saying what it is:
            // `!` someone else can type, `r` read-only.
            let (badge, row, _) = badged(s, 2, false);
            let want = match (Badge::of(s).unwrap().warn, s.seat) {
                (true, _) => "◉!",
                (false, Seat::Viewer) => "◉r",
                (false, _) => "◉ ",
            };
            assert_eq!(row, want, "{s:?}: {badge:?}");
        }
        let mut b = busy_bar();
        let viewer = sharing(Seat::Viewer, 1, 0);
        append_sharing(&mut b, Some(viewer), &Theme::DEFAULT, 2, "*", false);
        let row: String = b.render_row(2).iter().map(|c| c.ch).collect();
        assert_eq!(row, "*r", "NO_COLOR");
    }

    /// The counts give way before any other segment does.
    #[test]
    fn counts_go_before_other_segments() {
        let s = sharing(Seat::Host, 0, 2);
        let full = badged(s, 200, false);
        assert_eq!(full.0, "◉ shared · 2 watching ");
        let others = |b: &StatusBar| {
            let mut all: Vec<String> = b
                .left
                .iter()
                .chain(&b.center)
                .chain(&b.right)
                .filter(|s| s.group != BADGE)
                .map(|s| s.text.clone())
                .collect();
            all.sort();
            all
        };
        let width = StatusBar::segments_width;
        let need = |b: &StatusBar| width(&b.left) + width(&b.center) + width(&b.right) + 2;
        let tight = u16::try_from(need(&full.2) - 1).unwrap();
        let (badge, _, b) = badged(s, tight, false);
        assert_eq!(badge, "◉ shared ");
        assert_eq!(
            others(&b),
            others(&full.2),
            "a segment went before the count"
        );
    }

    /// The badge's own colours: amber for a host who is only watched, red and
    /// bold for a warning; monochrome strips every colour off the row.
    #[test]
    fn the_badge_takes_the_theme_and_monochrome_strips_it() {
        let rgb = |Rgb { r, g, b }: Rgb| Some(Color::Rgb { r, g, b });
        let (_, _, watched) = badged(sharing(Seat::Host, 0, 1), 120, false);
        assert!(
            watched
                .right
                .iter()
                .filter(|s| s.group == BADGE)
                .all(|s| s.fg == rgb(Theme::DEFAULT.status_shared) && !s.bold)
        );
        let (_, _, mut warned) = badged(sharing(Seat::Writer, 1, 0), 120, false);
        assert!(
            warned
                .right
                .iter()
                .filter(|s| s.group == BADGE)
                .all(|s| s.fg == rgb(Theme::DEFAULT.status_error) && s.bold)
        );
        warned.bg = Some(Color::Black);
        monochrome(&mut warned);
        assert!(warned.bg.is_none());
        assert!(
            warned
                .left
                .iter()
                .chain(&warned.center)
                .chain(&warned.right)
                .all(|s| s.fg.is_none() && s.bg.is_none())
        );
        let mut star = busy_bar();
        append_sharing(
            &mut star,
            Some(sharing(Seat::Viewer, 1, 0)),
            &Theme::DEFAULT,
            120,
            "*",
            false,
        );
        assert_eq!(star.right[0].text, "*");
    }

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
            action_error: None,
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

    #[test]
    fn a_failed_action_replaces_the_centre_in_the_error_colour() {
        let (snap, sid, wid, pid, meta) = fixture();
        let onb = OnboardingState::default();
        let mut c = ctx(sid, "project", wid, pid, &meta, &onb, 120);
        c.last_action = Some(("pane split", Instant::now()));
        c.action_error = Some("split failed: No such file or directory ([shell].command `x`)");
        let bar = build(&snap, &Theme::DEFAULT, &c);
        assert_eq!(
            flat(&bar.center),
            " split failed: No such file or directory ([shell].command `x`) "
        );
        let e = Theme::DEFAULT.status_error;
        assert_eq!(
            bar.center[0].fg,
            Some(Color::Rgb {
                r: e.r,
                g: e.g,
                b: e.b
            })
        );
    }

    #[test]
    fn a_notice_that_fits_clears_nothing() {
        let notice = " [pane not keeping up] ";
        for (left, right) in [(" left ", ""), ("", " right ")] {
            let mut b = bar(left, notice, right);
            let cols = (left.len() + notice.len() + right.len() + 1) as u16;
            make_room_for_notice(&mut b, cols);
            assert_eq!(
                (b.left.len(), b.right.len()),
                (
                    usize::from(!left.is_empty()),
                    usize::from(!right.is_empty())
                ),
                "a bar that already fit lost a segment"
            );
        }
    }

    #[test]
    fn a_notice_that_does_not_fit_clears_the_right_zone_first() {
        let notice = " [pane not keeping up] ";
        let mut b = bar(" left ", notice, " right ");
        make_room_for_notice(&mut b, (6 + notice.len() + 2) as u16);
        assert!(b.right.is_empty() && !b.left.is_empty());
        let row: String = b
            .render_row((6 + notice.len() + 2) as u16)
            .iter()
            .map(|c| c.ch)
            .collect();
        assert!(row.contains(notice.trim()), "{row:?}");
    }
}
