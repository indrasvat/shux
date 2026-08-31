//! The snapshot render path: compose a window's panes into one frame,
//! rasterize it, and hand back the PNG.
//!
//! `pane.snapshot`, `window.snapshot` and `session.snapshot` all land here, so
//! the status bar a snapshot draws is built by the same `statusbar_build`
//! renderer the live attach loop uses — a PNG that disagreed with an attached
//! client would be the whole bug this path exists to avoid.

use std::sync::Arc;

use tokio::sync::Mutex;

use crate::pane_io::PaneIoState;
use crate::{onboarding, session_meta, statusbar_build, statusbar_runner};

/// Build the OOTB status bar for a snapshot frame, using the same
/// `statusbar_build::build` renderer the live attach path uses so the
/// PNG matches what a fresh attached client would see.
///
/// The snapshot path doesn't have a live attach context, so we
/// synthesize a `StatusBarCtx` with defaults that mirror "fresh OOTB
/// experience": onboarding state read from the daemon-loaded handle,
/// session_meta read from the cache, no live `last_action`, no
/// copy-mode flag, no daemon-uptime (snapshots are stateless).
///
/// `segments` carries the latest script-driven `[[statusbar.segment]]`
/// outputs; `populate_bar` appends them into the same StatusBar the
/// attach loop assembles, so PNG snapshots match what an attached
/// client renders.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn build_snapshot_status_bar(
    snap: &shux_core::graph::SessionGraphSnapshot,
    session_id: &shux_core::model::SessionId,
    window_id: shux_core::model::WindowId,
    cols: u16,
    config: &shux_core::config::ConfigHandle,
    meta_cache: &session_meta::SessionMetaCache,
    onboarding: &onboarding::OnboardingHandle,
    segments: &statusbar_runner::SegmentCache,
) -> shux_ui::StatusBar {
    let theme = {
        let cfg = config.current();
        shux_core::theme::Theme::resolve(&cfg.theme)
    };
    let live_cfg = config.current();
    let nerd_fonts = live_cfg.appearance.nerd_fonts;
    let prefix_label = statusbar_build::prefix_display(&live_cfg.keys.prefix);
    let session_meta = meta_cache.get(*session_id).await;
    let onboarding_state = onboarding.current().await;

    // The active pane id is what the live attach path would show as
    // the focus. For the snapshot we read from the graph.
    let active_pane_id = snap
        .windows
        .get(&window_id)
        .map(|w| w.active_pane)
        .unwrap_or_default();

    let session_name = snap
        .sessions
        .get(session_id)
        .map(|s| s.name.clone())
        .unwrap_or_default();

    let ctx = statusbar_build::StatusBarCtx {
        session_id: *session_id,
        session_name: &session_name,
        active_window_id: window_id,
        active_pane_id,
        session_meta: &session_meta,
        onboarding: &onboarding_state,
        daemon_uptime: std::time::Duration::from_secs(0),
        nerd_fonts,
        prefix_label: &prefix_label,
        client_cols: cols,
        copy_mode_active: false,
        last_action: None,
    };
    // Bridge the cold-start race the attach path doesn't suffer from:
    // when a snapshot fires right after daemon start or a config reload,
    // the runner tasks may not have completed their first tick yet, so
    // `populate_bar` would read an empty cache and silently emit no
    // segments. Wait up to 1.2s for every configured segment index to
    // have a cache entry; on timeout we proceed anyway so a slow / hung
    // command can't wedge the RPC. The 1.2s budget slightly exceeds the
    // runner's per-command 1s timeout so the runner's fallback-bytes
    // write has room to land before we give up (codex round-4 nit).
    // Codex-bot P2, PR #45.
    let segment_count = live_cfg.statusbar.segment.len();
    if segment_count > 0 {
        let _ = segments
            .wait_for_first_outputs(segment_count, std::time::Duration::from_millis(1200))
            .await;
    }
    let mut bar = statusbar_build::build(snap, &theme, &ctx);
    // Append script-driven `[[statusbar.segment]]` outputs the same
    // way the attach render loop does. Without this, PNG snapshots
    // would only show the built-in OOTB segments and silently drop
    // every user-configured segment.
    statusbar_runner::populate_bar(&mut bar, config, segments).await;
    bar
}

/// Tail-clip captured text for inclusion in wait_for response previews.
/// Keeps the LAST `n` chars (matches are usually near the bottom of the
/// captured viewport) and trims leading whitespace.
pub(crate) fn preview_for_log(s: &str, n: usize) -> String {
    let bytes = s.as_bytes();
    if bytes.len() <= n {
        return s.trim_start().to_string();
    }
    let start = bytes.len() - n;
    let mut s = std::str::from_utf8(&bytes[start..])
        .unwrap_or("")
        .to_string();
    if let Some(idx) = s.find('\n') {
        s = s.split_off(idx + 1);
    }
    s.trim_start().to_string()
}

/// Parse optional `cols` / `rows` from snapshot params. Defaults: 120x36.
/// Same range guard as `pane.set_size`.
pub(crate) fn parse_snapshot_dims(
    params: &serde_json::Value,
) -> Result<(u16, u16), shux_rpc::RpcError> {
    let cols = params.get("cols").and_then(|v| v.as_u64()).unwrap_or(120);
    let rows = params.get("rows").and_then(|v| v.as_u64()).unwrap_or(36);
    if !(4..=1000).contains(&cols) || !(2..=1000).contains(&rows) {
        return Err(shux_rpc::RpcError::invalid_params(&format!(
            "rows/cols out of range (got rows={rows} cols={cols}; \
             valid: 4..=1000 cols, 2..=1000 rows)"
        )));
    }
    Ok((cols as u16, rows as u16))
}

/// One pane's render-clone payload, captured under the pane-IO lock:
/// (id, grid clone with resolved defaults, cursor, dynamic default colors).
pub(crate) type PaneSnapshotData = (
    shux_core::model::PaneId,
    shux_vt::Grid,
    shux_vt::Cursor,
    shux_vt::TerminalDefaultColors,
);

/// Per-pane lens ContentRevision map, captured in the SAME io-lock critical
/// section as the grid clones (PR #87 bot P1: same-lock, no VT-side tear).
pub(crate) type PaneRevisions = std::collections::HashMap<shux_core::model::PaneId, u64>;

/// Compose every pane in `window_id` into a single ComposedFrame at
/// `cols × rows`, rasterize it, and return the JSON `pane.snapshot`-shaped
/// response (with `window_id` in place of `pane_id`).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn snapshot_window(
    // The caller's graph snapshot — taken ONCE and shared with any metadata
    // the caller derives (lens council P1 major 5: session.snapshot's
    // session_version/panes[] and the rendered window must come from the
    // same snapshot, or concurrent structural mutation yields torn output).
    snap: &shux_core::graph::SessionGraphSnapshot,
    io: &Arc<Mutex<PaneIoState>>,
    window_id: shux_core::model::WindowId,
    cols: u16,
    rows: u16,
    rasterizer: Arc<shux_raster::Rasterizer>,
    config: &shux_core::config::ConfigHandle,
    meta_cache: &session_meta::SessionMetaCache,
    onboarding: &onboarding::OnboardingHandle,
    segments: &statusbar_runner::SegmentCache,
    // Pane ids whose `content_revision` must be captured in the SAME io-lock
    // critical section as the VT grid clones (PR #87 bot P1: a second lock
    // read at T2 let an old PNG pair with a newer revision). Returned as the
    // second tuple element; pass `&[]` when revisions aren't needed.
    revision_panes: &[shux_core::model::PaneId],
) -> Result<(serde_json::Value, PaneRevisions), shux_rpc::RpcError> {
    let (cw, ch) = rasterizer.cell_size();
    let pixel_count = (cols as u64)
        .saturating_mul(cw as u64)
        .saturating_mul(rows as u64)
        .saturating_mul(ch as u64);
    const MAX_PIXELS: u64 = 16_000_000;
    if pixel_count > MAX_PIXELS {
        return Err(shux_rpc::RpcError::invalid_params(&format!(
            "snapshot would be {pixel_count} pixels — exceeds cap of {MAX_PIXELS}"
        )));
    }

    let window = snap
        .windows
        .get(&window_id)
        .ok_or_else(|| shux_rpc::RpcError::not_found("window", &window_id.to_string()))?;

    // Build per-pane title map from the graph (priority-resolved values).
    let mut titles: std::collections::HashMap<shux_core::model::PaneId, String> =
        std::collections::HashMap::new();
    for pid in window.layout.tree.pane_ids() {
        if let Some(p) = snap.panes.get(&pid)
            && !p.title.is_empty()
        {
            titles.insert(pid, p.title.clone());
        }
    }

    // Snapshot just the (Grid, Cursor, dynamic colors) per pane under the io
    // lock — VT itself isn't Clone and we want to release the lock before
    // rasterizing. The caller's `revision_panes` content_revisions are read
    // inside the SAME critical section so the rendered pixels and the
    // published revisions are provably same-lock (no VT-side tear).
    let (pane_data, revisions): (Vec<PaneSnapshotData>, PaneRevisions) = {
        let state = io.lock().await;
        let pane_data = window
            .layout
            .tree
            .pane_ids()
            .into_iter()
            .filter_map(|pid| {
                state.vts.get(&pid).map(|vt| {
                    let mut grid = vt.grid().clone();
                    let default_colors = vt.default_colors();
                    resolve_grid_default_colors(&mut grid, default_colors);
                    (pid, grid, vt.cursor().clone(), default_colors)
                })
            })
            .collect();
        let revisions = revision_panes
            .iter()
            .filter_map(|pid| state.vts.get(pid).map(|vt| (*pid, vt.content_revision())))
            .collect();
        (pane_data, revisions)
    };

    let focused = window.active_pane;
    let layout_tree = window.layout.tree.clone();
    let zoom_state = window.layout.zoom.clone();

    // Build the same status bar `shux attach` would render so the snapshot
    // matches what a user sees attached. We don't have the live attached
    // state here, so we synthesize the StatusBarCtx with snapshot-time
    // defaults — every signal that does have a daemon-side source
    // (git branch, onboarding hint, theme, nerd-fonts toggle) IS still
    // populated, so PNGs honestly reflect the OOTB experience.
    let status_bar = build_snapshot_status_bar(
        snap,
        &window.session_id,
        window_id,
        cols,
        config,
        meta_cache,
        onboarding,
        segments,
    )
    .await;
    const STATUS_BAR_ROWS: u16 = 1;

    // Compose with the user's outline style: `compose` derives the pane viewport
    // from it, so a hardcoded style crops panes under `border_style = "none"`.
    // Read here, before `spawn_blocking` takes the closure.
    let border_style = shux_ui::BorderStyle::parse(&config.current().appearance.border_style);

    let (img, png_buf) = tokio::task::spawn_blocking(move || {
        let panes: std::collections::HashMap<
            shux_core::model::PaneId,
            (&shux_vt::Grid, &shux_vt::Cursor),
        > = pane_data.iter().map(|(p, g, c, _)| (*p, (g, c))).collect();
        let focused_defaults = pane_data
            .iter()
            .find(|(pid, _, _, _)| *pid == focused)
            .map(|(_, _, _, defaults)| *defaults)
            .unwrap_or_default();
        let focused_cursor_shape = pane_data
            .iter()
            .find(|(pid, _, _, _)| *pid == focused)
            .map(|(_, _, cursor, _)| cursor.shape)
            .unwrap_or_default();
        let inputs = shux_ui::ComposeInputs {
            layout: &layout_tree,
            zoom: zoom_state.as_ref(),
            focused,
            panes: &panes,
            titles: Some(&titles),
            status_bar: Some(&status_bar),
        };
        let composed = shux_ui::compose(
            &inputs,
            cols,
            rows,
            border_style,
            shux_ui::BorderColors::default(),
            STATUS_BAR_ROWS,
        );
        let opts = shux_raster::RasterOptions {
            cursor: composed.cursor,
            cursor_shape: focused_cursor_shape,
            cursor_color: focused_defaults.cursor,
            ..Default::default()
        };
        let img = rasterizer.render(&composed.grid, &opts);
        let mut buf: Vec<u8> = Vec::with_capacity(128 * 1024);
        {
            use image::ImageEncoder;
            let encoder = image::codecs::png::PngEncoder::new(&mut buf);
            encoder
                .write_image(
                    img.as_raw(),
                    img.width(),
                    img.height(),
                    image::ExtendedColorType::Rgba8,
                )
                .map_err(|e| format!("PNG encode failed: {e}"))?;
        }
        Ok::<_, String>((img, buf))
    })
    .await
    .map_err(|e| shux_rpc::RpcError::internal(&format!("rasterize join: {e}")))?
    .map_err(|e| shux_rpc::RpcError::internal(&e))?;

    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&png_buf);

    Ok((
        serde_json::json!({
            "window_id": window_id.to_string(),
            "png_base64": b64,
            "width": img.width(),
            "height": img.height(),
            "cell_width": cw,
            "cell_height": ch,
            "cols": cols,
            "rows": rows,
            "format": "png",
        }),
        revisions,
    ))
}

pub(crate) fn resolve_grid_default_colors(
    grid: &mut shux_vt::Grid,
    defaults: shux_vt::TerminalDefaultColors,
) {
    if defaults.fg.is_none() && defaults.bg.is_none() {
        return;
    }
    for row_idx in 0..grid.rows() {
        let mut row = grid.visible_row_mut(row_idx);
        for col_idx in 0..row.len() {
            let cell = &mut row[col_idx];
            if cell.style.fg == shux_vt::Color::Default
                && let Some([r, g, b]) = defaults.fg
            {
                cell.style.fg = shux_vt::Color::Rgb(r, g, b);
            }
            if cell.style.bg == shux_vt::Color::Default
                && let Some([r, g, b]) = defaults.bg
            {
                cell.style.bg = shux_vt::Color::Rgb(r, g, b);
            }
        }
    }
}

pub(crate) enum SnapshotFontBytes {
    Static(&'static [u8]),
    Owned(Vec<u8>),
}

impl SnapshotFontBytes {
    fn as_slice(&self) -> &[u8] {
        match self {
            Self::Static(bytes) => bytes,
            Self::Owned(bytes) => bytes,
        }
    }
}

/// Build the snapshot rasterizer from current config.
///
/// - `appearance.font` unset → bundled JBM-NF primary + default fallback chain.
/// - `appearance.font` set + file readable + font parseable → that
///   font as primary, then configured/default fallbacks.
/// - `appearance.font` set BUT unreadable or unparseable → returns
///   `Err`. The hot-reload caller's `Err` branch keeps the last-good
///   rasterizer; the startup caller logs the error and falls back to the
///   bundled chain so snapshot RPCs still return PNGs.
/// - `appearance.font_fallbacks` omitted → default builtin fallback
///   tokens. Set explicitly → exact ordered fallback chain after the
///   primary font. Empty lists are rejected. When `appearance.font` is
///   unset, the bundled JBM-NF font remains the primary metrics anchor
///   and the explicit list is used strictly as glyph fallback coverage.
///
/// Council review (PR #46): the previous behaviour silently fell back
/// to the bundled chain on bad custom-font paths, contradicting the
/// "keep last good rasterizer" comment in the hot-reload spawn and
/// making the `Err` branch of the reload loop unreachable for the
/// most common failure mode.
pub(crate) fn build_snapshot_rasterizer(
    cfg: &shux_core::config::Config,
) -> Result<shux_raster::Rasterizer, shux_raster::RasterError> {
    let primary = match cfg.appearance.font.as_ref() {
        None => None,
        Some(path) => Some(std::fs::read(path).map_err(|e| {
            shux_raster::RasterError::Font(format!(
                "appearance.font: read {} failed: {e}",
                path.display()
            ))
        })?),
    };
    let explicit_fallback_specs = cfg.appearance.font_fallbacks.clone();
    if explicit_fallback_specs.as_ref().is_some_and(Vec::is_empty) {
        return Err(shux_raster::RasterError::Font(
            "appearance.font_fallbacks must not be empty; omit it to use the default fallback chain"
                .into(),
        ));
    }
    let mut fallback_specs = explicit_fallback_specs.unwrap_or_else(|| {
        shux_raster::DEFAULT_FALLBACK_FONT_SPECS
            .iter()
            .map(|spec| (*spec).to_string())
            .collect()
    });
    let mut bundled_primary = None;
    if primary.is_none() {
        bundled_primary = Some(
            shux_raster::builtin_font_bytes(shux_raster::BUILTIN_NERD_FONT)
                .expect("builtin nerd font token should resolve"),
        );
        if fallback_specs
            .first()
            .is_some_and(|spec| spec == shux_raster::BUILTIN_NERD_FONT)
        {
            fallback_specs.remove(0);
        }
    }
    let fallback_fonts: Vec<SnapshotFontBytes> = fallback_specs
        .iter()
        .map(|spec| {
            if let Some(bytes) = shux_raster::builtin_font_bytes(spec) {
                Ok(SnapshotFontBytes::Static(bytes))
            } else if spec.starts_with("builtin:") {
                Err(shux_raster::RasterError::Font(format!(
                    "appearance.font_fallbacks: unknown builtin font token {spec:?}; expected one of {}",
                    shux_raster::DEFAULT_FALLBACK_FONT_SPECS.join(", ")
                )))
            } else {
                std::fs::read(spec)
                    .map(SnapshotFontBytes::Owned)
                    .map_err(|e| {
                        shux_raster::RasterError::Font(format!(
                            "appearance.font_fallbacks: read {spec} failed: {e}"
                        ))
                    })
            }
        })
        .collect::<Result<_, _>>()?;

    let fallback_refs = fallback_fonts.iter().map(SnapshotFontBytes::as_slice);
    let primary_ref = primary.as_deref().or(bundled_primary);
    shux_raster::Rasterizer::with_primary_and_fallback_fonts(14.0, primary_ref, fallback_refs)
}

/// Cheap equality-key for the subset of config that affects the
/// rasterizer chain. Returning the same value across two config
/// reloads means we can skip the rebuild — most reloads (border
/// styles, theme tweaks, statusbar segments) don't touch fonts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SnapshotFontKey {
    primary: Option<std::path::PathBuf>,
    fallbacks: Option<Vec<String>>,
}

pub(crate) fn snapshot_font_key(cfg: &shux_core::config::Config) -> SnapshotFontKey {
    SnapshotFontKey {
        primary: cfg.appearance.font.clone(),
        fallbacks: cfg.appearance.font_fallbacks.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `shux-pty` declares a cell box to pane children but cannot depend on
    /// `shux-raster`; this crate depends on both, so this is where they are held
    /// to one number. Built through `build_snapshot_rasterizer`, not a restated
    /// `14.0`, so it also fails if the default font or font size moves.
    #[test]
    fn declared_pty_cell_box_matches_the_default_snapshot_rasterizer() {
        let rasterizer = build_snapshot_rasterizer(&Config::default())
            .expect("default config must build a rasterizer");
        let (w, h) = rasterizer.cell_size();
        // `try_from`, not `as`: an `as` cast truncates 65545 to 9 and passes.
        let measured = (
            u16::try_from(w).expect("cell width fits u16"),
            u16::try_from(h).expect("cell height fits u16"),
        );
        assert_eq!(
            shux_pty::DECLARED_CELL_PIXELS,
            measured,
            "the cell box shux declares to pane children has drifted from the \
             one the default snapshot rasterizer actually renders"
        );
    }
    use shux_core::config::{Config, ConfigHandle, SegmentDef, StatusBarConfig};
    use shux_core::graph::SessionGraphSnapshot;
    use shux_core::model::{Pane, Session, Window};
    fn config_with_segment(zone: &str) -> ConfigHandle {
        let cfg = Config {
            statusbar: StatusBarConfig {
                left: None,
                center: None,
                right: None,
                segment: vec![SegmentDef {
                    zone: zone.to_string(),
                    command: vec!["echo".to_string()],
                    env: Default::default(),
                    starship_config: None,
                    interval_ms: 1_000,
                    fallback: None,
                }],
            },
            ..Default::default()
        };
        // The cache is pre-populated by the test so the command never
        // runs; we only need `handle.current()` to return our cfg. Use
        // `replace()` to seed it directly — avoids round-tripping a
        // tempfile through TOML serialize/parse just to exercise an
        // in-memory accessor. Pass a never-existing path so
        // `load_or_default` takes the NotFound branch on every platform.
        let nonexistent = std::env::temp_dir().join("__shux_test_no_such_config__.toml");
        let handle = ConfigHandle::load_or_default(&nonexistent);
        handle.replace(cfg);
        handle
    }

    fn snap_with_one_session() -> (
        SessionGraphSnapshot,
        shux_core::model::SessionId,
        shux_core::model::WindowId,
    ) {
        let pane = Pane::new(shux_core::model::WindowId::new(), "/");
        let mut window = Window::new(shux_core::model::SessionId::new(), "0", pane.id);
        // Fix up cross-refs: Window::new and Pane::new each minted their
        // own ids; pane.window_id must match window.id, window.session_id
        // must match session.id.
        let session_id = shux_core::model::SessionId::new();
        window.session_id = session_id;
        let mut pane = pane;
        pane.window_id = window.id;
        let session = Session::new("test", window.id);
        let session = Session {
            id: session_id,
            ..session
        };

        let mut snap = SessionGraphSnapshot::default();
        snap.sessions.insert(session.id, session);
        snap.windows.insert(window.id, window.clone());
        snap.panes.insert(pane.id, pane);
        (snap, session_id, window.id)
    }

    fn shux_raster_asset(name: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../shux-raster/assets")
            .join(name)
    }

    #[test]
    fn snapshot_rasterizer_default_chain_covers_tui_text_symbols() {
        let cfg = Config::default();
        let rasterizer = build_snapshot_rasterizer(&cfg).expect("rasterizer");
        for ch in ['\u{21bb}', '\u{2839}'] {
            assert!(
                rasterizer.has_glyph(ch),
                "default snapshot chain should resolve {ch:?}"
            );
            assert!(
                rasterizer.glyph_pixel_count(ch) >= 8,
                "default snapshot chain should render {ch:?} as non-empty pixels"
            );
        }
    }

    #[test]
    fn snapshot_rasterizer_accepts_ordered_builtin_fallbacks() {
        let mut cfg = Config::default();
        cfg.appearance.font_fallbacks = Some(vec![
            shux_raster::BUILTIN_SYMBOLS.to_string(),
            shux_raster::BUILTIN_MATH.to_string(),
            shux_raster::BUILTIN_SYMBOLS_LEGACY.to_string(),
            shux_raster::BUILTIN_EMOJI.to_string(),
        ]);

        let rasterizer = build_snapshot_rasterizer(&cfg).expect("rasterizer");
        let baseline = shux_raster::Rasterizer::new(14.0).expect("baseline rasterizer");
        assert_eq!(
            rasterizer.cell_size(),
            baseline.cell_size(),
            "custom fallbacks without appearance.font must not replace primary metrics"
        );
        assert!(rasterizer.has_glyph('\u{21bb}'));
        assert!(rasterizer.has_glyph('\u{2839}'));
        assert!(rasterizer.has_glyph('\u{1f37a}'));
    }

    #[test]
    fn snapshot_rasterizer_accepts_path_fallback_without_replacing_primary_metrics() {
        let mut cfg = Config::default();
        cfg.appearance.font = Some(shux_raster_asset("JetBrainsMonoNerdFontMono-Regular.ttf"));
        cfg.appearance.font_fallbacks = Some(vec![
            shux_raster_asset("NotoSansSymbols2-Regular.ttf")
                .display()
                .to_string(),
        ]);

        let rasterizer = build_snapshot_rasterizer(&cfg).expect("rasterizer");
        let baseline = shux_raster::Rasterizer::with_primary_font(
            14.0,
            include_bytes!("../../shux-raster/assets/JetBrainsMonoNerdFontMono-Regular.ttf"),
        )
        .expect("baseline rasterizer");
        assert_eq!(
            rasterizer.cell_size(),
            baseline.cell_size(),
            "fallback chain must not change the explicit primary font metrics"
        );
        assert!(rasterizer.has_glyph('A'));
        assert!(rasterizer.has_glyph('\u{2839}'));
    }

    #[test]
    fn snapshot_rasterizer_rejects_empty_fallbacks() {
        let mut cfg = Config::default();
        cfg.appearance.font_fallbacks = Some(vec![]);

        let err = match build_snapshot_rasterizer(&cfg) {
            Ok(_) => panic!("empty fallback list should fail"),
            Err(err) => err,
        };
        assert!(
            err.to_string().contains("must not be empty"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn snapshot_rasterizer_rejects_unknown_builtin_fallback_token() {
        let mut cfg = Config::default();
        cfg.appearance.font_fallbacks = Some(vec!["builtin:symbol".to_string()]);

        let err = match build_snapshot_rasterizer(&cfg) {
            Ok(_) => panic!("unknown builtin fallback token should fail"),
            Err(err) => err,
        };
        assert!(
            err.to_string().contains("unknown builtin font token"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn snapshot_rasterizer_rejects_missing_fallback_path() {
        let mut cfg = Config::default();
        cfg.appearance.font_fallbacks = Some(vec![
            "/tmp/this-shux-font-fallback-does-not-exist.ttf".to_string(),
        ]);

        let err = match build_snapshot_rasterizer(&cfg) {
            Ok(_) => panic!("missing fallback path should fail"),
            Err(err) => err,
        };
        assert!(
            err.to_string().contains("appearance.font_fallbacks"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn snapshot_font_key_tracks_fallback_changes() {
        let mut before = Config::default();
        before.appearance.font = Some(std::path::PathBuf::from("/tmp/primary.ttf"));
        let mut after = before.clone();
        after.appearance.font_fallbacks = Some(vec![shux_raster::BUILTIN_SYMBOLS.to_string()]);

        assert_ne!(snapshot_font_key(&before), snapshot_font_key(&after));
    }

    #[tokio::test]
    async fn snapshot_statusbar_includes_script_segments() {
        // Use the test-only OnboardingHandle constructor — no env
        // mutation, no filesystem. Process env is shared mutable state
        // across `cargo test` threads, so any env-mutating test risks
        // racing every other env-mutating test in the same binary
        // (codex round-2 P1: this would race
        // `onboarding::tests::round_trip_dismissal`).
        let onb = onboarding::OnboardingHandle::from_state_for_test(
            onboarding::OnboardingState::default(),
        );
        let config = config_with_segment("right");
        let meta = session_meta::SessionMetaCache::new();
        let segments = statusbar_runner::SegmentCache::new();
        segments
            .set_for_test(0, b"shux-test-sentinel".to_vec())
            .await;

        let (snap, session_id, window_id) = snap_with_one_session();

        let bar = build_snapshot_status_bar(
            &snap,
            &session_id,
            window_id,
            120,
            &config,
            &meta,
            &onb,
            &segments,
        )
        .await;

        let right_text: String = bar.right.iter().map(|s| s.text.clone()).collect();
        assert!(
            right_text.contains("shux-test-sentinel"),
            "expected snapshot status bar's right zone to contain the \
             segment sentinel, got: {right_text:?}"
        );
    }
}

#[cfg(test)]
mod declared_cell_pixels_pin {
    /// `shux-pty` DECLARES this box to pane children through
    /// `ws_xpixel`/`ws_ypixel`; `shux-vt` derives an image's cell footprint
    /// from it and `shux-raster` scales the pixels through it. Nothing in the
    /// type system links the two declarations, and this crate is the only one
    /// that sees both. Drift here mis-sizes every inline image with no other
    /// test failing.
    #[test]
    fn shux_vt_and_shux_pty_declare_the_same_cell_box() {
        let (pw, ph) = shux_pty::DECLARED_CELL_PIXELS;
        assert_eq!(
            (u32::from(pw), u32::from(ph)),
            shux_vt::DECLARED_CELL_PIXELS,
            "the declared cell box drifted between shux-pty and shux-vt"
        );
    }
}
