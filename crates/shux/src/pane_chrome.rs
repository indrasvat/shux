//! The border chrome every render path draws. One builder, so live attach
//! and the window/session snapshots cannot disagree about a pane's title or
//! status badge.

use std::collections::HashMap;

use shux_core::config::ProgramStatusConfig;
use shux_core::graph::SessionGraphSnapshot;
use shux_core::model::{Pane, PaneId};
use shux_core::theme::{Rgb, Theme};
use shux_program_status::State;
use shux_ui::{PaneChrome, StatusBadge};

/// Badge glyphs: (Unicode set, Nerd Font set). Every glyph is in the bundled
/// font and one cell wide.
fn glyphs(state: State) -> (char, char) {
    match state {
        State::Blocked => ('◆', '\u{f071}'),
        State::Error => ('✕', '\u{f00d}'),
        State::Done => ('✓', '\u{f00c}'),
        State::Working => ('◔', '\u{f110}'),
        State::Idle => ('○', '○'),
    }
}

fn color(state: State, theme: &Theme) -> Rgb {
    match state {
        State::Blocked => theme.status_attention,
        State::Error => theme.status_error,
        State::Done => theme.status_ok,
        State::Working | State::Idle => theme.status_muted,
    }
}

/// The chrome for `panes`: their titles, and a status badge where the
/// program reported one and the feature and its border badge are on.
pub fn pane_chrome(
    snap: &SessionGraphSnapshot,
    panes: impl IntoIterator<Item = PaneId>,
    status: &ProgramStatusConfig,
    nerd_fonts: bool,
    theme: &Theme,
) -> HashMap<PaneId, PaneChrome> {
    panes
        .into_iter()
        .filter_map(|pid| {
            let pane = snap.panes.get(&pid)?;
            let chrome = PaneChrome {
                title: pane.title.clone(),
                badge: badge(pane, status, nerd_fonts, theme),
            };
            (!chrome.title.is_empty() || chrome.badge.is_some()).then_some((pid, chrome))
        })
        .collect()
}

/// The config is checked as well as the pane: an exited pane keeps its final
/// status even after the feature is switched off.
fn badge(
    pane: &Pane,
    status: &ProgramStatusConfig,
    nerd_fonts: bool,
    theme: &Theme,
) -> Option<StatusBadge> {
    if !(status.enabled && status.border && pane.program_status_enabled) {
        return None;
    }
    let state = pane.program_status.as_ref()?.state;
    let (unicode, nerd) = glyphs(state);
    let Rgb { r, g, b } = color(state, theme);
    Some(StatusBadge {
        glyph: if nerd_fonts { nerd } else { unicode },
        color: crossterm::style::Color::Rgb { r, g, b },
    })
}

/// Panes in `session_id` whose program is blocked on the user, when the
/// feature and its status-bar segment are on.
pub fn waiting_count(
    snap: &SessionGraphSnapshot,
    session_id: shux_core::model::SessionId,
    status: &ProgramStatusConfig,
) -> usize {
    if !(status.enabled && status.status_bar) {
        return 0;
    }
    let Some(session) = snap.sessions.get(&session_id) else {
        return 0;
    };
    session
        .windows
        .iter()
        .filter_map(|wid| snap.windows.get(wid))
        .flat_map(|w| w.layout.tree.pane_ids())
        .filter_map(|pid| snap.panes.get(&pid))
        .filter(|p| {
            p.program_status_enabled
                && p.program_status.as_ref().map(|s| s.state) == Some(State::Blocked)
        })
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::test_harness::{RpcHarness, dispatch_ok};
    use base64::Engine;
    use shux_core::model::{Session, SessionId, Window};
    use shux_program_status::{StateCounts, StatusSummary};

    fn summary(state: State) -> StatusSummary {
        StatusSummary {
            state,
            kind: None,
            progress: None,
            app: None,
            id: None,
            counts: StateCounts::default(),
        }
    }

    /// A session of one window holding one pane per state (None: never
    /// reported), all titled "p".
    fn session(states: &[Option<State>]) -> (SessionGraphSnapshot, SessionId, Vec<PaneId>) {
        let session_id = SessionId::new();
        let mut snap = SessionGraphSnapshot::default();
        let mut panes: Vec<Pane> = states
            .iter()
            .map(|state| {
                let mut p = Pane::new(shux_core::model::WindowId::new(), "/");
                p.title = "p".into();
                p.program_status = state.map(summary);
                p.program_status_enabled = true;
                p
            })
            .collect();
        let mut window = Window::new(session_id, "0", panes[0].id);
        for p in &mut panes {
            p.window_id = window.id;
        }
        for p in &panes[1..] {
            assert!(window.layout.split_pane(
                panes[0].id,
                p.id,
                shux_core::layout::Direction::Vertical,
                0.5
            ));
        }
        let session = Session {
            id: session_id,
            ..Session::new("s", window.id)
        };
        snap.sessions.insert(session_id, session);
        snap.windows.insert(window.id, window);
        let ids = panes.iter().map(|p| p.id).collect();
        for p in panes {
            snap.panes.insert(p.id, p);
        }
        (snap, session_id, ids)
    }

    fn badge_of(
        snap: &SessionGraphSnapshot,
        pane: PaneId,
        cfg: &ProgramStatusConfig,
        nerd: bool,
    ) -> Option<StatusBadge> {
        pane_chrome(snap, [pane], cfg, nerd, &Theme::DEFAULT)
            .remove(&pane)
            .and_then(|c| c.badge)
    }

    #[test]
    fn each_state_has_its_glyph_and_colour_in_both_sets() {
        let rgb = |c: Rgb| crossterm::style::Color::Rgb {
            r: c.r,
            g: c.g,
            b: c.b,
        };
        let t = Theme::DEFAULT;
        for (state, unicode, nerd, color) in [
            (State::Blocked, '◆', '\u{f071}', t.status_attention),
            (State::Error, '✕', '\u{f00d}', t.status_error),
            (State::Done, '✓', '\u{f00c}', t.status_ok),
            (State::Working, '◔', '\u{f110}', t.status_muted),
            (State::Idle, '○', '○', t.status_muted),
        ] {
            let (snap, _, panes) = session(&[Some(state)]);
            let cfg = ProgramStatusConfig::default();
            assert_eq!(
                badge_of(&snap, panes[0], &cfg, false),
                Some(StatusBadge {
                    glyph: unicode,
                    color: rgb(color)
                }),
                "{state:?}"
            );
            assert_eq!(
                badge_of(&snap, panes[0], &cfg, true).map(|b| b.glyph),
                Some(nerd)
            );
            for glyph in [unicode, nerd] {
                assert_eq!(
                    unicode_width::UnicodeWidthChar::width(glyph),
                    Some(1),
                    "{state:?} {glyph} is not one cell wide"
                );
            }
        }
    }

    #[test]
    fn the_badge_needs_the_feature_its_toggle_and_the_pane_all_on() {
        let (mut snap, _, panes) = session(&[Some(State::Blocked)]);
        let on = ProgramStatusConfig::default();
        assert!(badge_of(&snap, panes[0], &on, false).is_some());
        let off = ProgramStatusConfig {
            enabled: false,
            ..on.clone()
        };
        assert!(
            badge_of(&snap, panes[0], &off, false).is_none(),
            "feature off"
        );
        let no_border = ProgramStatusConfig {
            border: false,
            ..on.clone()
        };
        assert!(
            badge_of(&snap, panes[0], &no_border, false).is_none(),
            "border off"
        );
        let no_bar = ProgramStatusConfig {
            status_bar: false,
            ..on.clone()
        };
        assert!(
            badge_of(&snap, panes[0], &no_bar, false).is_some(),
            "the bar toggle is not the border's"
        );
        snap.panes
            .get_mut(&panes[0])
            .unwrap()
            .program_status_enabled = false;
        assert!(
            badge_of(&snap, panes[0], &on, false).is_none(),
            "pane reports off"
        );
    }

    #[test]
    fn a_pane_with_neither_title_nor_badge_has_no_chrome() {
        let (mut snap, _, panes) = session(&[None]);
        snap.panes.get_mut(&panes[0]).unwrap().title.clear();
        assert!(
            pane_chrome(
                &snap,
                [panes[0]],
                &ProgramStatusConfig::default(),
                false,
                &Theme::DEFAULT
            )
            .is_empty()
        );
    }

    #[test]
    fn waiting_counts_blocked_panes_in_this_session_only() {
        let (mut snap, session_id, _) = session(&[
            Some(State::Blocked),
            Some(State::Working),
            Some(State::Blocked),
            None,
        ]);
        let (other, _, _) = session(&[Some(State::Blocked)]);
        snap.sessions.extend(other.sessions);
        snap.windows.extend(other.windows);
        snap.panes.extend(other.panes);
        let on = ProgramStatusConfig::default();
        assert_eq!(waiting_count(&snap, session_id, &on), 2);
        let no_bar = ProgramStatusConfig {
            status_bar: false,
            ..on.clone()
        };
        assert_eq!(waiting_count(&snap, session_id, &no_bar), 0);
        let off = ProgramStatusConfig {
            enabled: false,
            ..on
        };
        assert_eq!(waiting_count(&snap, session_id, &off), 0);
    }

    async fn window_png(h: &RpcHarness, window: shux_core::model::WindowId) -> image::RgbImage {
        let v = dispatch_ok(
            &h.router,
            "window.snapshot",
            serde_json::json!({ "window_id": window.to_string(), "cols": 40, "rows": 10 }),
        )
        .await;
        let png = base64::engine::general_purpose::STANDARD
            .decode(v["png_base64"].as_str().unwrap())
            .unwrap();
        image::load_from_memory(&png).unwrap().to_rgb8()
    }

    /// A blocked pane shows its badge and the "waiting" segment in the
    /// attention colour; switching both off in the live config removes them
    /// from the very next snapshot, without the program reporting again.
    #[tokio::test]
    async fn a_window_snapshot_shows_a_blocked_pane_until_the_toggles_go_off() {
        let h = RpcHarness::new();
        let (_s, window, pane) = h.seed_session("badge").await;
        let _input = h.seed_io(pane, b"x").await;
        h.graph
            .set_pane_program_status(
                pane,
                shux_core::graph::PaneStatusUpdate {
                    generation: 1,
                    enabled: true,
                    summary: Some(summary(State::Blocked)),
                    seen: true,
                },
            )
            .await
            .unwrap();
        let Rgb { r, g, b } = Theme::DEFAULT.status_attention;
        let attention = |img: &image::RgbImage| img.pixels().filter(|p| p.0 == [r, g, b]).count();
        assert!(
            attention(&window_png(&h, window).await) > 0,
            "no badge or segment drawn"
        );

        let mut cfg = shux_core::config::Config::default();
        cfg.program_status.border = false;
        cfg.program_status.status_bar = false;
        h.config.replace(cfg.clone());
        assert_eq!(
            attention(&window_png(&h, window).await),
            0,
            "still drawn with both off"
        );

        cfg.program_status.border = true;
        cfg.program_status.status_bar = true;
        h.config.replace(cfg);
        assert!(
            attention(&window_png(&h, window).await) > 0,
            "not back after re-enabling"
        );
        h.stop().await;
    }

    /// The live view draws borders in the theme's colours; a window snapshot
    /// must draw the same ones.
    #[tokio::test]
    async fn a_window_snapshot_draws_borders_in_the_theme_colours() {
        let h = RpcHarness::new();
        let (_s, window, pane) = h.seed_session("theme-borders").await;
        let _input = h.seed_io(pane, b"x").await;
        let mut cfg = shux_core::config::Config::default();
        cfg.theme.border_focused = Some("#ff0000".into());
        cfg.theme.border_unfocused = Some("#ff0000".into());
        h.config.replace(cfg);
        let img = window_png(&h, window).await;
        let red = img.pixels().filter(|p| p.0 == [255, 0, 0]).count();
        assert!(red > 0, "no border pixel in the theme's #ff0000");
        h.stop().await;
    }
}
