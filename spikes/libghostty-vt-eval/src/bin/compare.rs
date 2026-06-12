use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use image::{ImageBuffer, Rgba, RgbaImage};
use libghostty_vt::render::{CellIterator, CursorVisualStyle, RowIterator};
use libghostty_vt::screen::CellWide;
use libghostty_vt::style::{RgbColor, Style, StyleColor, Underline};
use libghostty_vt::{RenderState, Terminal, TerminalOptions};
use shux_raster::{RasterOptions, Rasterizer};
use shux_vt::{Cell, CellFlags, CellStyle, Color, CursorShape, Grid, GridConfig, VirtualTerminal};

const DEFAULT_COLS: u16 = 96;
const DEFAULT_ROWS: u16 = 28;
const FONT_SIZE: f32 = 14.0;

#[derive(Clone)]
struct Fixture {
    name: String,
    description: String,
    input: Vec<u8>,
    cols: u16,
    rows: u16,
    resize_to: Option<(u16, u16)>,
}

struct BackendRender {
    grid: Grid,
    opts: RasterOptions,
    text: String,
    duration_ms: f64,
}

struct CaseResult {
    name: String,
    description: String,
    cell_diff_count: usize,
    compared_cells: usize,
    pixel_diff_count: usize,
    compared_pixels: usize,
    mean_abs_channel_delta: f64,
    default_color_diff: bool,
    shux_duration_ms: f64,
    ghostty_duration_ms: f64,
    contact_path: PathBuf,
    shux_text: String,
    ghostty_text: String,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut out_dir = PathBuf::from(".shux/out/libghostty-vt-replacement");
    let mut extra_recordings = Vec::new();
    let mut args = env::args().skip(1);

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--out" => {
                out_dir = PathBuf::from(args.next().ok_or("--out requires a path")?);
            }
            "--recording" => {
                let spec = args.next().ok_or("--recording requires name:path")?;
                let (name, path) = spec
                    .split_once(':')
                    .ok_or("--recording must have the form name:path")?;
                extra_recordings.push((name.to_owned(), PathBuf::from(path)));
            }
            "-h" | "--help" => {
                print_help();
                return Ok(());
            }
            other => return Err(format!("unknown argument: {other}").into()),
        }
    }

    fs::create_dir_all(&out_dir)?;
    let rasterizer = Rasterizer::new(FONT_SIZE)?;
    let mut fixtures = built_in_fixtures();
    for (name, path) in extra_recordings {
        let input = fs::read(&path)?;
        fixtures.push(Fixture {
            name,
            description: format!("raw PTY recording from {}", path.display()),
            input,
            cols: 120,
            rows: 36,
            resize_to: None,
        });
    }

    let mut results = Vec::new();
    for fixture in fixtures {
        results.push(run_case(&rasterizer, &fixture, &out_dir)?);
    }

    write_report(&out_dir, &results)?;
    println!(
        "wrote {} cases to {}",
        results.len(),
        out_dir.canonicalize()?.display()
    );
    Ok(())
}

fn print_help() {
    println!(
        "usage: compare [--out DIR] [--recording name:path]\n\
         \n\
         Generates shux-vt vs libghostty-vt screenshots, diffs, contact sheets, and report.md."
    );
}

fn built_in_fixtures() -> Vec<Fixture> {
    vec![
        fixture(
            "plain",
            "plain text, carriage returns, and line feeds",
            b"hello\r\nworld",
        ),
        fixture(
            "sgr-truecolor",
            "bold/italic/underline plus truecolor foreground/background",
            b"\x1b[1;3;4;38;2;12;34;56;48;2;90;80;70mstyled\x1b[0m normal",
        ),
        fixture(
            "ansi-palette",
            "16-color and 256-color SGR palette output",
            b"\x1b[31mred\x1b[0m \x1b[92mbright-green\x1b[0m \x1b[38;5;202midx202\x1b[0m",
        ),
        fixture(
            "osc-default-bg",
            "OSC 11 default background color using #RRGGBB syntax",
            b"\x1b]11;#1E1E2E\x07default-bg",
        ),
        fixture(
            "cursor-color-shape",
            "OSC 12 cursor color and bar cursor shape",
            b"\x1b]12;#00ff80\x07\x1b[5 qcursor",
        ),
        fixture(
            "cjk-wide",
            "CJK wide cells mixed with ASCII",
            "A你B 界 C".as_bytes(),
        ),
        fixture(
            "combining-mark",
            "combining acute accent after ASCII base character",
            "Cafe\u{301} resume\u{301}".as_bytes(),
        ),
        fixture(
            "extended-emoji",
            "ZWJ and skin-tone emoji sequences",
            "rainbow 🏳️\u{200d}🌈 thumbs 👍🏽 tool 🛠️".as_bytes(),
        ),
        fixture(
            "alternate-screen",
            "enter alternate screen, write content, leave alternate screen",
            b"primary\x1b[?1049h\x1b[2J\x1b[Halt-screen\x1b[?1049lback",
        ),
        fixture(
            "scroll-region",
            "scroll margins with indexed lines",
            b"\x1b[2;5r\x1b[Hone\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix\x1b[r",
        ),
        fixture(
            "sync-output",
            "DCS synchronized output hold and release",
            b"before\x1bP=1s\x1b\\hidden\x1bP=2s\x1b\\after",
        ),
        Fixture {
            name: "resize-reflow".to_owned(),
            description: "main-screen wrapped content after resize".to_owned(),
            input: b"abcdef ghijkl mnopqr stuvwx yz".to_vec(),
            cols: 18,
            rows: 6,
            resize_to: Some((8, 8)),
        },
    ]
}

fn fixture(name: &str, description: &str, input: &[u8]) -> Fixture {
    Fixture {
        name: name.to_owned(),
        description: description.to_owned(),
        input: input.to_vec(),
        cols: DEFAULT_COLS,
        rows: DEFAULT_ROWS,
        resize_to: None,
    }
}

fn run_case(
    rasterizer: &Rasterizer,
    fixture: &Fixture,
    out_dir: &Path,
) -> Result<CaseResult, Box<dyn std::error::Error>> {
    let shux = render_shux(fixture)?;
    let ghostty = render_ghostty(fixture)?;

    let shux_img = rasterizer.render(&shux.grid, &shux.opts);
    let ghostty_img = rasterizer.render(&ghostty.grid, &ghostty.opts);
    let diff_img = diff_image(&shux_img, &ghostty_img);
    let contact = contact_sheet(&shux_img, &ghostty_img, &diff_img);

    let shux_path = out_dir.join(format!("{}-shux.png", fixture.name));
    let ghostty_path = out_dir.join(format!("{}-ghostty.png", fixture.name));
    let diff_path = out_dir.join(format!("{}-diff.png", fixture.name));
    let contact_path = out_dir.join(format!("{}-contact.png", fixture.name));
    shux_img.save(&shux_path)?;
    ghostty_img.save(&ghostty_path)?;
    diff_img.save(&diff_path)?;
    contact.save(&contact_path)?;

    let (cell_diff_count, compared_cells) = grid_diff_count(&shux.grid, &ghostty.grid);
    let pixel_stats = pixel_stats(&shux_img, &ghostty_img);

    Ok(CaseResult {
        name: fixture.name.clone(),
        description: fixture.description.clone(),
        cell_diff_count,
        compared_cells,
        pixel_diff_count: pixel_stats.0,
        compared_pixels: pixel_stats.1,
        mean_abs_channel_delta: pixel_stats.2,
        default_color_diff: shux.opts.fg_default != ghostty.opts.fg_default
            || shux.opts.bg_default != ghostty.opts.bg_default
            || shux.opts.cursor_color != ghostty.opts.cursor_color,
        shux_duration_ms: shux.duration_ms,
        ghostty_duration_ms: ghostty.duration_ms,
        contact_path,
        shux_text: shux.text,
        ghostty_text: ghostty.text,
    })
}

fn render_shux(fixture: &Fixture) -> Result<BackendRender, Box<dyn std::error::Error>> {
    let started = Instant::now();
    let mut vt = VirtualTerminal::new(fixture.rows as usize, fixture.cols as usize);
    vt.process(&fixture.input);
    if let Some((cols, rows)) = fixture.resize_to {
        vt.resize(rows as usize, cols as usize);
    }
    let duration_ms = started.elapsed().as_secs_f64() * 1000.0;
    let colors = vt.default_colors();
    let cursor = vt.cursor();
    let opts = RasterOptions {
        fg_default: colors.fg.unwrap_or([220, 220, 220]),
        bg_default: colors.bg.unwrap_or([16, 16, 24]),
        cursor: cursor.visible.then_some((cursor.row, cursor.col)),
        cursor_shape: cursor.shape,
        cursor_color: colors.cursor,
    };
    Ok(BackendRender {
        grid: vt.grid().clone_visible(),
        opts,
        text: vt.capture_text(None),
        duration_ms,
    })
}

fn render_ghostty(fixture: &Fixture) -> Result<BackendRender, Box<dyn std::error::Error>> {
    let started = Instant::now();
    let mut terminal = Terminal::new(TerminalOptions {
        cols: fixture.cols,
        rows: fixture.rows,
        max_scrollback: 5_000,
    })?;
    terminal.vt_write(&fixture.input);
    if let Some((cols, rows)) = fixture.resize_to {
        terminal.resize(cols, rows, 8, 16)?;
    }

    let mut render_state = RenderState::new()?;
    let snapshot = render_state.update(&terminal)?;
    let grid = ghostty_snapshot_to_grid(&snapshot)?;
    let colors = snapshot.colors()?;
    let baseline = ghostty_baseline_colors(fixture.cols, fixture.rows)?;
    let cursor = snapshot.cursor_viewport()?;
    let duration_ms = started.elapsed().as_secs_f64() * 1000.0;
    let opts = RasterOptions {
        fg_default: if colors.foreground == baseline.foreground {
            [220, 220, 220]
        } else {
            rgb(colors.foreground)
        },
        bg_default: if colors.background == baseline.background {
            [16, 16, 24]
        } else {
            rgb(colors.background)
        },
        cursor: cursor.map(|pos| (pos.y as usize, pos.x as usize)),
        cursor_shape: cursor_shape(snapshot.cursor_visual_style()?),
        cursor_color: snapshot.cursor_color()?.map(rgb),
    };
    let text = grid_text(&grid);
    Ok(BackendRender {
        grid,
        opts,
        text,
        duration_ms,
    })
}

fn ghostty_baseline_colors(
    cols: u16,
    rows: u16,
) -> Result<libghostty_vt::render::Colors, Box<dyn std::error::Error>> {
    let terminal = Terminal::new(TerminalOptions {
        cols,
        rows,
        max_scrollback: 0,
    })?;
    let mut render_state = RenderState::new()?;
    Ok(render_state.update(&terminal)?.colors()?)
}

fn ghostty_snapshot_to_grid(
    snapshot: &libghostty_vt::render::Snapshot<'_, '_>,
) -> Result<Grid, Box<dyn std::error::Error>> {
    let rows = snapshot.rows()? as usize;
    let cols = snapshot.cols()? as usize;
    let mut grid = Grid::new(rows, cols, GridConfig { max_scrollback: 0 });
    let mut row_iter = RowIterator::new()?;
    let mut cell_iter = CellIterator::new()?;
    let mut row_iteration = row_iter.update(snapshot)?;
    let mut r = 0;

    while let Some(row) = row_iteration.next() {
        if r >= rows {
            break;
        }
        let mut cells = cell_iter.update(row)?;
        let mut c = 0;
        while let Some(cell) = cells.next() {
            if c >= cols {
                break;
            }
            let raw = cell.raw_cell()?;
            let wide = raw.wide()?;
            grid.visible_row_mut(r)[c] = match wide {
                CellWide::SpacerTail => Cell::wide_continuation(),
                CellWide::SpacerHead => Cell::EMPTY,
                CellWide::Narrow | CellWide::Wide => {
                    let style = cell.style()?;
                    let graphemes: String = cell.graphemes()?.into_iter().collect();
                    Cell {
                        ch: graphemes.chars().next().unwrap_or(' '),
                        width: if wide == CellWide::Wide { 2 } else { 1 },
                        style: map_style(style),
                        extended: None,
                    }
                }
            };
            c += 1;
        }
        r += 1;
    }

    Ok(grid)
}

fn map_style(style: Style) -> CellStyle {
    let mut flags = CellFlags::default();
    if style.bold {
        flags.set(CellFlags::BOLD);
    }
    if style.faint {
        flags.set(CellFlags::DIM);
    }
    if style.italic {
        flags.set(CellFlags::ITALIC);
    }
    if style.underline != Underline::None {
        flags.set(CellFlags::UNDERLINE);
    }
    if style.blink {
        flags.set(CellFlags::BLINK);
    }
    if style.inverse {
        flags.set(CellFlags::INVERSE);
    }
    if style.invisible {
        flags.set(CellFlags::HIDDEN);
    }
    if style.strikethrough {
        flags.set(CellFlags::STRIKETHROUGH);
    }

    CellStyle {
        fg: map_color(style.fg_color),
        bg: map_color(style.bg_color),
        flags,
    }
}

fn map_color(color: StyleColor) -> Color {
    match color {
        StyleColor::None => Color::Default,
        StyleColor::Palette(index) => Color::Indexed(index.0),
        StyleColor::Rgb(color) => Color::Rgb(color.r, color.g, color.b),
    }
}

fn cursor_shape(style: CursorVisualStyle) -> CursorShape {
    match style {
        CursorVisualStyle::Bar => CursorShape::Bar,
        CursorVisualStyle::Underline => CursorShape::Underline,
        CursorVisualStyle::Block | CursorVisualStyle::BlockHollow => CursorShape::Block,
        _ => CursorShape::Block,
    }
}

fn rgb(color: RgbColor) -> [u8; 3] {
    [color.r, color.g, color.b]
}

fn grid_text(grid: &Grid) -> String {
    let mut text = String::new();
    let last_content = (0..grid.rows())
        .rev()
        .find(|&r| !grid.visible_row(r).is_blank())
        .unwrap_or(0);
    for row_idx in 0..=last_content {
        let row = grid.visible_row(row_idx);
        let mut line = String::new();
        for c in 0..row.len() {
            let cell = &row[c];
            if !cell.is_wide_continuation() {
                line.push(cell.ch);
            }
        }
        text.push_str(line.trim_end());
        text.push('\n');
    }
    text
}

fn grid_diff_count(left: &Grid, right: &Grid) -> (usize, usize) {
    let rows = left.rows().min(right.rows());
    let cols = left.cols().min(right.cols());
    let mut diffs = 0;
    for r in 0..rows {
        let lrow = left.visible_row(r);
        let rrow = right.visible_row(r);
        for c in 0..cols.min(lrow.len()).min(rrow.len()) {
            let left = &lrow[c];
            let right = &rrow[c];
            if left.ch != right.ch
                || left.width != right.width
                || left.style != right.style
                || left.is_wide_continuation() != right.is_wide_continuation()
            {
                diffs += 1;
            }
        }
    }
    (diffs, rows * cols)
}

fn pixel_stats(left: &RgbaImage, right: &RgbaImage) -> (usize, usize, f64) {
    let w = left.width().min(right.width());
    let h = left.height().min(right.height());
    let mut changed = 0;
    let mut total_delta: u64 = 0;
    for y in 0..h {
        for x in 0..w {
            let lp = left.get_pixel(x, y).0;
            let rp = right.get_pixel(x, y).0;
            if lp != rp {
                changed += 1;
            }
            for i in 0..3 {
                total_delta += lp[i].abs_diff(rp[i]) as u64;
            }
        }
    }
    let pixels = (w * h) as usize;
    let mean = if pixels == 0 {
        0.0
    } else {
        total_delta as f64 / (pixels as f64 * 3.0)
    };
    (changed, pixels, mean)
}

fn diff_image(left: &RgbaImage, right: &RgbaImage) -> RgbaImage {
    let w = left.width().min(right.width());
    let h = left.height().min(right.height());
    let mut diff = ImageBuffer::from_pixel(w, h, Rgba([10, 10, 14, 255]));
    for y in 0..h {
        for x in 0..w {
            let lp = left.get_pixel(x, y).0;
            let rp = right.get_pixel(x, y).0;
            let delta = lp[0]
                .abs_diff(rp[0])
                .max(lp[1].abs_diff(rp[1]))
                .max(lp[2].abs_diff(rp[2]));
            if delta > 0 {
                diff.put_pixel(x, y, Rgba([255, delta.max(80), 40, 255]));
            }
        }
    }
    diff
}

fn contact_sheet(left: &RgbaImage, right: &RgbaImage, diff: &RgbaImage) -> RgbaImage {
    let gap = 12;
    let w = left.width() + right.width() + diff.width() + gap * 4;
    let h = left.height().max(right.height()).max(diff.height()) + gap * 2;
    let mut sheet = ImageBuffer::from_pixel(w, h, Rgba([24, 24, 28, 255]));
    overlay(&mut sheet, left, gap, gap);
    overlay(&mut sheet, right, left.width() + gap * 2, gap);
    overlay(
        &mut sheet,
        diff,
        left.width() + right.width() + gap * 3,
        gap,
    );
    sheet
}

fn overlay(dst: &mut RgbaImage, src: &RgbaImage, x0: u32, y0: u32) {
    for y in 0..src.height() {
        for x in 0..src.width() {
            dst.put_pixel(x0 + x, y0 + y, *src.get_pixel(x, y));
        }
    }
}

fn write_report(out_dir: &Path, results: &[CaseResult]) -> Result<(), Box<dyn std::error::Error>> {
    let mut report = String::new();
    report.push_str("# libghostty-vt Replacement A/B Report\n\n");
    report.push_str("Same input bytes, same `shux-raster`; only the VT state backend differs.\n\n");
    report.push_str(
        "| Case | Cell diff | Pixel diff | Mean channel delta | Default colors | shux ms | ghostty ms | Contact |\n",
    );
    report.push_str("|---|---:|---:|---:|---|---:|---:|---|\n");
    for result in results {
        let cell_pct = pct(result.cell_diff_count, result.compared_cells);
        let pixel_pct = pct(result.pixel_diff_count, result.compared_pixels);
        report.push_str(&format!(
            "| `{}` | {} / {} ({:.2}%) | {} / {} ({:.2}%) | {:.2} | {} | {:.3} | {:.3} | [{}]({}) |\n",
            result.name,
            result.cell_diff_count,
            result.compared_cells,
            cell_pct,
            result.pixel_diff_count,
            result.compared_pixels,
            pixel_pct,
            result.mean_abs_channel_delta,
            if result.default_color_diff {
                "diff"
            } else {
                "same"
            },
            result.shux_duration_ms,
            result.ghostty_duration_ms,
            result
                .contact_path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("contact"),
            result
                .contact_path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
        ));
    }
    report.push_str("\n## Case Notes\n\n");
    for result in results {
        report.push_str(&format!(
            "### `{}`\n\n{}\n\n",
            result.name, result.description
        ));
        if result.shux_text != result.ghostty_text {
            report.push_str("Text differs after backend normalization.\n\n");
            report.push_str("```text\n");
            report.push_str("--- shux-vt\n");
            report.push_str(&result.shux_text);
            report.push_str("--- libghostty-vt\n");
            report.push_str(&result.ghostty_text);
            report.push_str("```\n\n");
        }
        if result.default_color_diff {
            report.push_str(
                "Default foreground/background/cursor state differs between backends.\n\n",
            );
        }
    }
    fs::write(out_dir.join("report.md"), report)?;
    Ok(())
}

fn pct(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 * 100.0 / denominator as f64
    }
}
