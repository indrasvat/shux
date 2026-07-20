use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use shux_raster::{RasterOptions, Rasterizer};
use shux_vt::VirtualTerminal;

const FONT_SIZE: f32 = 14.0;
const EXPECTED_PRIMARY_FONT: &str =
    "crates/shux-raster/assets/JetBrainsMonoNerdFontMono-Regular.ttf";
const DEFAULT_FG: [u8; 3] = [220, 220, 220];
const DEFAULT_BG: [u8; 3] = [16, 16, 24];

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Verify,
    Promote,
}

#[derive(Debug)]
struct Args {
    fixtures: PathBuf,
    goldens: PathBuf,
    out: PathBuf,
    mode: Mode,
}

#[derive(Debug, Deserialize)]
struct SyntheticManifest {
    fixtures: Vec<SyntheticFixture>,
}

#[derive(Debug, Deserialize)]
struct SyntheticFixture {
    name: String,
    description: String,
    init: Geometry,
    steps: Vec<SyntheticStep>,
    #[serde(default)]
    expected_responses_hex: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct SyntheticStep {
    #[serde(default)]
    process: Option<ProcessInput>,
    #[serde(default)]
    resize: Option<Geometry>,
}

#[derive(Debug, Deserialize)]
struct ProcessInput {
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    hex: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
struct Geometry {
    rows: usize,
    cols: usize,
}

#[derive(Debug, Deserialize)]
struct RichManifest {
    cols: usize,
    rows: usize,
    #[serde(default = "default_font_size")]
    font_size: f32,
    font: String,
    #[serde(default = "default_fg")]
    fg_default: [u8; 3],
    #[serde(default = "default_bg")]
    bg_default: [u8; 3],
    #[serde(default)]
    cursor_policy: CursorPolicy,
    fixtures: Vec<RichFixture>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CursorPolicy {
    #[default]
    Disabled,
}

#[derive(Debug, Deserialize)]
struct RichFixture {
    name: String,
    command: String,
    raw: String,
    bytes: u64,
    #[serde(rename = "sha256")]
    sha256: String,
    #[serde(default)]
    rows: Option<usize>,
    #[serde(default)]
    cols: Option<usize>,
}

#[derive(Debug, Serialize)]
struct CorpusReport {
    schema_version: u32,
    mode: String,
    cases: Vec<CaseReport>,
}

#[derive(Debug, Serialize)]
struct CaseReport {
    name: String,
    layer: String,
    description: String,
    status: String,
    text_status: String,
    responses_status: String,
    actual_text: String,
    expected_text: String,
    actual_png: String,
    expected_png: String,
    diff_png: String,
    rows: usize,
    cols: usize,
}

struct RenderedCase {
    name: String,
    layer: String,
    description: String,
    rows: usize,
    cols: usize,
    text: String,
    responses_hex: Vec<String>,
    expected_responses_hex: Vec<String>,
    image: image::RgbaImage,
}

fn default_font_size() -> f32 {
    FONT_SIZE
}

fn default_fg() -> [u8; 3] {
    DEFAULT_FG
}

fn default_bg() -> [u8; 3] {
    DEFAULT_BG
}

fn main() -> Result<()> {
    let args = parse_args()?;
    fs::create_dir_all(&args.out)?;
    fs::create_dir_all(&args.goldens)?;

    let rasterizer = Rasterizer::new(FONT_SIZE)?;
    let mut reports = Vec::new();
    let mut failures = Vec::new();

    for case in render_synthetic_cases(&args.fixtures, &rasterizer)? {
        let report = write_and_compare_case(&args, case, &mut failures)?;
        reports.push(report);
    }
    for case in render_rich_cases(&args.fixtures, &rasterizer)? {
        let report = write_and_compare_case(&args, case, &mut failures)?;
        reports.push(report);
    }

    let report = CorpusReport {
        schema_version: 1,
        mode: match args.mode {
            Mode::Verify => "verify".to_owned(),
            Mode::Promote => "promote".to_owned(),
        },
        cases: reports,
    };
    fs::write(
        args.out.join("corpus-report.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;

    if args.mode == Mode::Verify && !failures.is_empty() {
        return Err(format!("VT corpus failed: {}", failures.join("; ")).into());
    }

    println!("vt corpus {} complete: {}", report.mode, args.out.display());
    Ok(())
}

fn parse_args() -> Result<Args> {
    let mut fixtures = PathBuf::from(".shux/fixtures/vt-corpus");
    let mut goldens = PathBuf::from(".shux/goldens/073-vt-corpus");
    let mut out = PathBuf::from(".shux/out/073-vt-corpus/rendered");
    let mut mode = Mode::Verify;

    let mut iter = env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--fixtures" => fixtures = PathBuf::from(iter.next().ok_or("--fixtures needs path")?),
            "--goldens" => goldens = PathBuf::from(iter.next().ok_or("--goldens needs path")?),
            "--out" => out = PathBuf::from(iter.next().ok_or("--out needs path")?),
            "--mode" => {
                mode = match iter.next().ok_or("--mode needs verify|promote")?.as_str() {
                    "verify" => Mode::Verify,
                    "promote" => Mode::Promote,
                    other => return Err(format!("unknown mode: {other}").into()),
                };
            }
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {other}").into()),
        }
    }

    Ok(Args {
        fixtures,
        goldens,
        out,
        mode,
    })
}

fn print_help() {
    println!(
        "usage: cargo run -p shux-raster --example vt_corpus_harness -- \\\n+           [--mode verify|promote] [--fixtures DIR] [--goldens DIR] [--out DIR]"
    );
}

fn render_synthetic_cases(
    fixtures_root: &Path,
    rasterizer: &Rasterizer,
) -> Result<Vec<RenderedCase>> {
    let manifest_path = fixtures_root.join("synthetic/manifest.json");
    let manifest: SyntheticManifest = serde_json::from_slice(&fs::read(&manifest_path)?)?;
    let mut cases = Vec::new();

    for fixture in manifest.fixtures {
        let mut vt = VirtualTerminal::new(fixture.init.rows, fixture.init.cols);
        let mut responses = Vec::new();
        let mut rows = fixture.init.rows;
        let mut cols = fixture.init.cols;
        for step in &fixture.steps {
            if let Some(input) = &step.process {
                let bytes = input.to_bytes()?;
                responses.extend(
                    vt.process_with_responses(&bytes)
                        .into_iter()
                        .map(hex_encode),
                );
            }
            if let Some(size) = step.resize {
                vt.resize(size.rows, size.cols);
                rows = size.rows;
                cols = size.cols;
            }
        }
        let colors = vt.default_colors();
        let opts = RasterOptions {
            fg_default: colors.fg.unwrap_or(DEFAULT_FG),
            bg_default: colors.bg.unwrap_or(DEFAULT_BG),
            cursor: None,
            cursor_shape: vt.cursor().shape,
            cursor_color: colors.cursor,
        };
        let expected_responses_hex: Vec<String> = fixture
            .expected_responses_hex
            .into_iter()
            .map(|response| response.to_ascii_lowercase())
            .collect();
        if !expected_responses_hex.is_empty() && responses != expected_responses_hex {
            return Err(format!(
                "{} response mismatch: expected {:?}, got {:?}",
                fixture.name, expected_responses_hex, responses
            )
            .into());
        }
        cases.push(RenderedCase {
            name: fixture.name,
            layer: "synthetic".to_owned(),
            description: fixture.description,
            rows,
            cols,
            text: vt.capture_text(None),
            responses_hex: responses,
            expected_responses_hex,
            image: rasterizer.render(vt.grid(), &opts),
        });
    }

    Ok(cases)
}

fn render_rich_cases(fixtures_root: &Path, rasterizer: &Rasterizer) -> Result<Vec<RenderedCase>> {
    let rich_root = fixtures_root.join("rich-tui");
    let manifest: RichManifest =
        serde_json::from_slice(&fs::read(rich_root.join("manifest.json"))?)?;
    if (manifest.font_size - FONT_SIZE).abs() > f32::EPSILON {
        return Err(format!(
            "unsupported font size in rich manifest: {}",
            manifest.font_size
        )
        .into());
    }
    if manifest.font != EXPECTED_PRIMARY_FONT {
        return Err(format!(
            "unsupported primary font in rich manifest: {}",
            manifest.font
        )
        .into());
    }
    if !matches!(manifest.cursor_policy, CursorPolicy::Disabled) {
        return Err("only disabled cursor policy is supported for rich replay".into());
    }

    let mut cases = Vec::new();
    for fixture in manifest.fixtures {
        let rows = fixture.rows.unwrap_or(manifest.rows);
        let cols = fixture.cols.unwrap_or(manifest.cols);
        let raw_path = rich_root.join(&fixture.raw);
        let raw = fs::read(&raw_path)?;
        if raw.len() as u64 != fixture.bytes {
            return Err(format!(
                "{} byte mismatch: manifest={} actual={}",
                fixture.name,
                fixture.bytes,
                raw.len()
            )
            .into());
        }
        let actual_sha256 = hex_encode(Sha256::digest(&raw).to_vec());
        if actual_sha256 != fixture.sha256 {
            return Err(format!(
                "{} sha256 mismatch: manifest={} actual={}",
                fixture.name, fixture.sha256, actual_sha256
            )
            .into());
        }
        let mut vt = VirtualTerminal::new(rows, cols);
        vt.process(&raw);
        let opts = RasterOptions {
            fg_default: manifest.fg_default,
            bg_default: manifest.bg_default,
            cursor: None,
            cursor_shape: vt.cursor().shape,
            cursor_color: None,
        };
        cases.push(RenderedCase {
            name: fixture.name,
            layer: "rich-tui".to_owned(),
            description: fixture.command,
            rows,
            cols,
            text: vt.capture_text(None),
            responses_hex: Vec::new(),
            expected_responses_hex: Vec::new(),
            image: rasterizer.render(vt.grid(), &opts),
        });
    }
    Ok(cases)
}

fn write_and_compare_case(
    args: &Args,
    case: RenderedCase,
    failures: &mut Vec<String>,
) -> Result<CaseReport> {
    let case_id = format!("{}-{}", case.layer, case.name);
    let actual_text = args.out.join(format!("{case_id}-actual.txt"));
    let actual_png = args.out.join(format!("{case_id}-actual.png"));
    let expected_text = args.goldens.join(format!("{case_id}-expected.txt"));
    let expected_responses = args.goldens.join(format!("{case_id}-responses.json"));
    let expected_png = args.goldens.join(format!("{case_id}-expected.png"));
    let diff_png = args.out.join(format!("{case_id}-diff.png"));

    fs::write(&actual_text, case.text.as_bytes())?;
    case.image.save(&actual_png)?;

    if args.mode == Mode::Promote {
        fs::create_dir_all(&args.goldens)?;
        fs::write(&expected_text, case.text.as_bytes())?;
        let baseline_responses = if case.expected_responses_hex.is_empty() {
            &case.responses_hex
        } else {
            &case.expected_responses_hex
        };
        fs::write(
            &expected_responses,
            serde_json::to_vec_pretty(baseline_responses)?,
        )?;
        case.image.save(&expected_png)?;
    }

    let text_status = compare_text(&actual_text, &expected_text)?;
    if text_status != "pass" {
        failures.push(format!("{case_id} text {text_status}"));
    }
    let responses_status = compare_responses(&case.responses_hex, &expected_responses)?;
    if responses_status != "pass" {
        failures.push(format!("{case_id} responses {responses_status}"));
    }
    if args.mode == Mode::Verify && !expected_png.exists() {
        failures.push(format!("{case_id} png missing_baseline"));
    }

    Ok(CaseReport {
        name: case.name,
        layer: case.layer,
        description: case.description,
        status: if text_status == "pass" && responses_status == "pass" {
            "pending_pixel".to_owned()
        } else {
            "fail".to_owned()
        },
        text_status,
        responses_status,
        actual_text: display_path(&actual_text),
        expected_text: display_path(&expected_text),
        actual_png: display_path(&actual_png),
        expected_png: display_path(&expected_png),
        diff_png: display_path(&diff_png),
        rows: case.rows,
        cols: case.cols,
    })
}

fn compare_text(actual: &Path, expected: &Path) -> Result<String> {
    if !expected.exists() {
        return Ok("missing_baseline".to_owned());
    }
    Ok(if fs::read(actual)? == fs::read(expected)? {
        "pass"
    } else {
        "fail"
    }
    .to_owned())
}

fn compare_responses(actual: &[String], expected: &Path) -> Result<String> {
    if !expected.exists() {
        return Ok("missing_baseline".to_owned());
    }
    let expected: Vec<String> = serde_json::from_slice(&fs::read(expected)?)?;
    Ok(if actual == expected { "pass" } else { "fail" }.to_owned())
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

impl ProcessInput {
    fn to_bytes(&self) -> Result<Vec<u8>> {
        match (&self.text, &self.hex) {
            (Some(text), None) => Ok(text.as_bytes().to_vec()),
            (None, Some(hex)) => hex_decode(hex),
            (Some(_), Some(_)) => Err("process input must use either text or hex, not both".into()),
            (None, None) => Err("process input needs text or hex".into()),
        }
    }
}

fn hex_encode(bytes: Vec<u8>) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn hex_decode(hex: &str) -> Result<Vec<u8>> {
    let compact: String = hex.chars().filter(|ch| !ch.is_whitespace()).collect();
    if !compact.len().is_multiple_of(2) {
        return Err(format!("hex string has odd length: {hex}").into());
    }
    let mut out = Vec::with_capacity(compact.len() / 2);
    for idx in (0..compact.len()).step_by(2) {
        out.push(u8::from_str_radix(&compact[idx..idx + 2], 16)?);
    }
    Ok(out)
}
