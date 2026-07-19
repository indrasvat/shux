//! CLI argument definitions and subcommand dispatch.
//!
//! Every `shux` subcommand is a thin wrapper over a JSON-RPC call to the daemon
//! (PRD §4.3 invariant 2: "CLI == API").

use std::path::PathBuf;

use crate::features::plugin::{PluginScaffoldRuntime, ScaffoldOptions};
use clap::builder::styling::{AnsiColor, Effects, Styles};
use clap::{Parser, Subcommand, ValueEnum};

const CLAP_STYLES: Styles = Styles::styled()
    .header(AnsiColor::Cyan.on_default().effects(Effects::BOLD))
    .usage(AnsiColor::Cyan.on_default().effects(Effects::BOLD))
    .literal(AnsiColor::Green.on_default().effects(Effects::BOLD))
    .placeholder(AnsiColor::Yellow.on_default())
    .valid(AnsiColor::Green.on_default())
    .invalid(AnsiColor::Red.on_default().effects(Effects::BOLD))
    .error(AnsiColor::Red.on_default().effects(Effects::BOLD));

/// Render the long-form agent reference block appended to `shux --help`.
///
/// The same content is emitted twice — once with shux's brand colours
/// baked in via ANSI escapes (terracotta accent for headers + `shux`
/// commands, green for RPC methods, dim for inline comments), and once
/// as plain text with all escapes stripped. The colour decision honours
/// `NO_COLOR=…` (any value) and falls back to plain when stdout isn't
/// a TTY, matching the same `IsTerminal` check the rest of the CLI uses.
/// Long-form `about` text shown at the top of `shux --help`. Adapts to
/// NO_COLOR + IsTerminal so plain piped output stays clean; brand-tinted
/// when emitted to a real terminal. Returns plain text with optional ANSI
/// escapes embedded.
pub fn long_about() -> String {
    use std::io::IsTerminal;
    let force =
        std::env::var_os("CLICOLOR_FORCE").is_some() || std::env::var_os("FORCE_COLOR").is_some();
    let no_color = std::env::var_os("NO_COLOR").is_some();
    let colorize = !no_color && (force || std::io::stdout().is_terminal());
    render_long_about(colorize)
}

fn render_long_about(colorize: bool) -> String {
    let acc = if colorize {
        "\x1b[1;38;2;215;108;58m"
    } else {
        ""
    }; // bold terracotta
    let dim = if colorize { "\x1b[2m" } else { "" };
    let bold = if colorize { "\x1b[1m" } else { "" };
    let mono = if colorize {
        "\x1b[38;2;180;175;160m"
    } else {
        ""
    }; // warm pale gray for inline code
    let r = if colorize { "\x1b[0m" } else { "" };

    let sx = format!("{acc}shux{r}");
    let bul = format!("{dim}·{r}");

    let mut s = String::with_capacity(512);
    s.push_str(&format!(
        "{sx} is a terminal multiplexer (sessions / windows / panes, like tmux) \
        for humans and AI agents.\n\n"
    ));
    s.push_str(&format!(
        "{bold}Typed JSON-RPC surface (UDS + TCP) with:{r}\n"
    ));
    s.push_str(&format!("  {bul} atomic declarative workspace templates\n"));
    s.push_str(&format!("  {bul} optimistic concurrency on every entity\n"));
    s.push_str(&format!("  {bul} sealed PTY-output event bus\n"));
    s.push_str(&format!(
        "  {bul} built-in PNG rasterizer {dim}— any pane, no terminal in the loop{r}\n\n"
    ));
    s.push_str(&format!(
        "Every CLI subcommand mirrors an RPC method 1:1 — RPC dots become CLI \
         spaces ({mono}session.create{r} → {mono}shux session create{r}). Drive raw \
         RPCs directly via {mono}`shux rpc call <method> --params @file`{r} \
         (also accepts {mono}-{r} for stdin and inline JSON).",
    ));
    s
}

pub fn agent_help() -> String {
    use std::io::IsTerminal;
    let colorize = std::env::var_os("NO_COLOR").is_none() && std::io::stdout().is_terminal();
    render_agent_help(colorize)
}

fn render_agent_help(colorize: bool) -> String {
    // Brand palette in 24-bit truecolor — matches the landing page.
    let acc = if colorize {
        "\x1b[1;38;2;215;108;58m"
    } else {
        ""
    }; // bold terracotta — section headers, `shux` brand
    let acc_dim = if colorize { "\x1b[38;2;199;90;42m" } else { "" }; // terracotta — `shux <verb>` ledes & URLs
    let cmd = if colorize {
        "\x1b[1;38;2;215;108;58m"
    } else {
        ""
    }; // bold terracotta — `shux` token
    let verb = if colorize { "\x1b[1;32m" } else { "" }; // bold green — subcommand verb
    let rpc = if colorize { "\x1b[32m" } else { "" }; // green — RPC method names
    let arrow = if colorize {
        "\x1b[38;2;146;138;120m"
    } else {
        ""
    }; // muted warm gray — →
    let dim = if colorize { "\x1b[2m" } else { "" }; // dim — inline comments
    let underline = if colorize { "\x1b[4m" } else { "" }; // underline — URLs
    let r = if colorize { "\x1b[0m" } else { "" }; // reset

    // Helper to render a `shux <verb>` token in two-tone colour.
    let shux = |v: &str| format!("{cmd}shux{r} {verb}{v}{r}");
    // Helper to render a section header.
    let h = |s: &str| format!("{acc}{s}{r}");
    // Helper to render an RPC method name.
    let m = |s: &str| format!("{rpc}{s}{r}");
    // Helper for arrows.
    let a = format!("{arrow}→{r}");
    // Helper for `shux` brand-name only.
    let sx = format!("{cmd}shux{r}");

    let mut s = String::with_capacity(4096);
    s.push_str(&format!("{}\n", h("COMMAND → RPC METHOD MAP")));
    s.push_str(&format!(
        "  {dim}RPC dots become CLI spaces. Every noun is namespaced.{r}\n\n"
    ));
    s.push_str(&format!(
        "  {:24} {a} {}\n",
        shux("session create"),
        m("session.create")
    ));
    s.push_str(&format!(
        "  {:24} {a} {}\n",
        shux("session list"),
        m("session.list")
    ));
    s.push_str(&format!(
        "  {:24} {a} {}\n",
        shux("session kill"),
        m("session.kill")
    ));
    s.push_str(&format!(
        "  {:24} {a} {}\n",
        shux("session rename"),
        m("session.rename")
    ));
    s.push_str(&format!(
        "  {:24} {a} {} {dim}(client-side, not RPC){r}\n",
        shux("session attach"),
        m("(attach)")
    ));
    s.push_str(&format!(
        "  {:24} {a} {}\n",
        shux("window <verb>"),
        m("window.{create,list,focus,kill,rename,reorder,ensure,snapshot}")
    ));
    s.push_str(&format!("  {:24} {a} {}\n", shux("pane <verb>"), m("pane.{send-keys,set-size,snapshot,capture,split,focus,zoom,swap,kill,set-title,resize,wait-for,output.watch,run}")));
    s.push_str(&format!(
        "  {:24} {a} {}\n",
        shux("plugin <verb>"),
        m("plugin.{install,list,kill,reload}")
    ));
    s.push_str(&format!(
        "  {:24} {a} {} / {}\n",
        shux("events <verb>"),
        m("events.history"),
        m("events.watch")
    ));
    s.push_str(&format!(
        "  {:24} {a} {} {dim}(atomic batch from a TOML template){r}\n",
        shux("state apply"),
        m("state.apply")
    ));
    s.push_str(&format!(
        "  {:24} {a} any method directly  {dim}(`--params @file` / `-` / inline){r}\n\n",
        shux("rpc call")
    ));

    s.push_str(&format!("{}\n", h("TYPICAL AGENT WORKFLOW")));
    s.push_str(&format!(
        "  {dim}# 1. Spawn a session in the caller's cwd running any command.{r}\n"
    ));
    s.push_str(&format!(
        "  {} demo --title demo -- lazygit\n",
        shux("session create"),
    ));
    s.push_str(&format!(
        "  {dim}# Raw RPC callers should pass cwd explicitly.{r}\n"
    ));
    s.push_str(&format!(
        "  {} --params \"{{\\\"name\\\":\\\"demo\\\",\\\"cwd\\\":\\\"$(pwd)\\\",\\\"command\\\":[\\\"lazygit\\\"]}}\"\n\n",
        shux("rpc call session.create"),
    ));
    s.push_str(&format!(
        "  {dim}# 2. Drive it. (Synchronous resize — next snapshot sees new dims.){r}\n"
    ));
    s.push_str(&format!(
        "  {} --params '{{\"pane_id\":\"$PID\",\"cols\":200,\"rows\":60}}'\n",
        shux("rpc call pane.set_size"),
    ));
    s.push_str(&format!(
        "  {} -s demo --text 'j'\n",
        shux("pane send-keys"),
    ));
    s.push_str(&format!(
        "  {} -s demo --data 'Gw=='   {dim}# Esc (base64){r}\n\n",
        shux("pane send-keys"),
    ));
    s.push_str(&format!(
        "  {dim}# 3. Pixel feedback (PNG, headless — no terminal emulator in the loop).{r}\n"
    ));
    s.push_str(&format!(
        "  {} --params '{{\"pane_id\":\"$PID\"}}' \\\n",
        shux("rpc call pane.snapshot"),
    ));
    s.push_str("    | jq -r .result.png_base64 | base64 -d > frame.png\n\n");
    s.push_str(&format!("  {dim}# Tear down when done.{r}\n"));
    s.push_str(&format!("  {} demo\n\n", shux("session kill")));

    s.push_str(&format!("{}\n", h("DECLARATIVE WORKSPACES")));
    s.push_str("  echo '[session]\n");
    s.push_str("  name=\"review\"\n");
    s.push_str("  [[windows]]\n");
    s.push_str("  title=\"git\"\n");
    s.push_str("  [[windows.panes]]\n");
    s.push_str("  command=[\"lazygit\"]' > spec.toml\n");
    s.push_str(&format!(
        "  {} spec.toml   {dim}# atomic; --dry-run prints the lowered ops{r}\n\n",
        shux("state apply"),
    ));

    s.push_str(&format!("{}\n", h("REPLACES THESE TOOLS")));
    let row = |tool: &str, with: &str| format!("  {tool:30} {a} {with}\n");
    s.push_str(&row(
        "tmux / screen / byobu",
        &format!("{} + {}", shux("state apply"), shux("session attach")),
    ));
    s.push_str(&row(
        "iTerm2 (Python SDK / AS)",
        &format!("{} + {}", m("pane.send_keys"), m("pane.snapshot")),
    ));
    s.push_str(&row(
        "expect / pexpect / sexpect",
        &format!(
            "{} {a} {} {a} {}",
            m("pane.send_keys"),
            m("pane.wait_for"),
            m("pane.capture")
        ),
    ));
    s.push_str(&row(
        "iTerm2 wait_for_text / _absent",
        &format!("{} {dim}(text · regex · --absent){r}", m("pane.wait_for")),
    ));
    s.push_str(&row(
        "asciinema rec / script(1)",
        &format!("{} {dim}(lossless raw PTY bytes){r}", m("pane.record")),
    ));
    s.push_str(&row(
        "vhs / agg / terminalizer",
        &format!("{} loop {a} ffmpeg", m("window.snapshot")),
    ));
    s.push_str(&row(
        "termshot / freezeframe",
        &format!("{} {dim}or{r} {}", m("pane.snapshot"), m("window.snapshot")),
    ));
    s.push_str(&row(
        "iTerm2 broadcast input",
        &format!("{} fan-out", m("pane.send_keys")),
    ));
    s.push_str(&row(
        "ttyrec / termsh",
        &format!("re-feed VT bytes {a} {}", m("pane.snapshot")),
    ));
    s.push_str(&row(
        "GNU parallel --tmux mode",
        "template with N panes + RPC orchestrator",
    ));
    s.push_str(&row(
        "Bubbletea / ratatui test harness",
        &format!("{} + golden-image diff", m("window.snapshot")),
    ));
    s.push('\n');

    let url = |u: &str| format!("{acc_dim}{underline}{u}{r}");
    s.push_str(&format!("{}\n", h("WHERE TO LEARN MORE")));
    s.push_str(&format!(
        "  Landing & live demos     {}\n",
        url("https://shux.pages.dev")
    ));
    s.push_str(&format!(
        "  Agent skill (drop-in)    {}\n",
        url("https://github.com/indrasvat/shux/tree/main/skills/shux")
    ));
    s.push_str(&format!(
        "  RPC reference            {}\n",
        url("https://github.com/indrasvat/shux/tree/main/skills/shux/references/api.md")
    ));
    s.push_str(&format!(
        "  Repository               {}\n\n",
        url("https://github.com/indrasvat/shux")
    ));

    s.push_str(&format!(
        "  Every entity in {sx} carries a 'version' field — pass 'expected_version' on\n"
    ));
    s.push_str(&format!(
        "  mutating RPCs for optimistic-concurrency rejection ({rpc}-32002{r}) on stale writes."
    ));

    s
}

/// shux — a modern, batteries-included terminal multiplexer
///
/// `after_long_help` is built lazily so we can show the longer agent
/// reference (workflows + RPC map + tools-replaced) on `--help` /
/// `-h --long` without inflating the default help screen.
#[derive(Parser, Debug)]
#[command(
    name = "shux",
    version,
    about = "A modern terminal multiplexer — works for humans, drives like an API",
    // long_about is injected at runtime in main() via cli::long_about() so it
    // can adapt to NO_COLOR / non-TTY stdout — clap's derive macro only
    // accepts a `&'static str` literal here. The plain-text fallback below
    // is what shows if someone uses Cli's derive output directly (e.g. tests).
    long_about = "shux is a terminal multiplexer for humans and AI agents.",
    // after_long_help is injected at runtime in main() so it can adapt
    // to NO_COLOR / non-TTY stdout. See `agent_help()`.
    after_help = "See 'shux <command> --help'.  For the full agent reference: 'shux --help'.",
    styles = CLAP_STYLES,
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Output format (text for humans, json for piping/scripting)
    #[arg(long, global = true, default_value = "text")]
    pub format: OutputFormat,

    /// Path to the daemon's Unix domain socket.
    /// Default: $XDG_RUNTIME_DIR/shux/shux.sock or /tmp/shux-$UID/shux.sock
    #[arg(long, global = true, env = "SHUX_SOCKET")]
    pub socket: Option<PathBuf>,

    /// Enable verbose logging (sets RUST_LOG=debug for this invocation)
    #[arg(short, long, global = true)]
    pub verbose: bool,
}

/// Output format for CLI commands.
#[derive(Debug, Clone, Copy, ValueEnum, Default)]
pub enum OutputFormat {
    /// Human-readable text output (default)
    #[default]
    Text,
    /// JSON output for scripting and piping
    Json,
    /// Plain tab-separated output for scripting (no box, no color)
    Plain,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Session lifecycle. Mirrors the `session.*` RPC namespace
    /// (`session.create` ↔ `shux session create`, etc.).
    #[command(visible_aliases = ["ses", "sess"])]
    Session {
        #[command(subcommand)]
        command: SessionCommand,
    },

    /// Window lifecycle and layout. Mirrors `window.*` RPC.
    #[command(alias = "win")]
    Window {
        #[command(subcommand)]
        command: WindowCommand,
    },

    /// Pane I/O, layout, and capture. Mirrors `pane.*` RPC.
    Pane {
        #[command(subcommand)]
        command: PaneCommand,
    },

    /// The lens composite verb — spawn a command in a hidden, self-cleaning
    /// scratch session. Mirrors `lens.run` RPC (lens PRD §8). `lens` is a
    /// CLI noun for exactly this ONE verb (`run`); the other four verbs of
    /// the run→settle→glance→drive→diff loop are pane primitives under
    /// `shux pane …` (`wait-settled`, `glance`, `send-keys`, `diff`) — see
    /// `shux lens --help` for the full recipe.
    Lens {
        #[command(subcommand)]
        command: LensCommand,
    },

    /// Process plugins (task 044a phase 0).
    ///
    /// `shux plugin install <path>` spawns an executable that speaks
    /// shux's line-delimited JSON-RPC dialect (see
    /// docs/tasks/044a-process-plugins-v0.md). The plugin can call
    /// any registered shux RPC method and subscribe to events
    /// declared in its `subscribes` manifest. Hot reload on file
    /// save is on by default.
    Plugin {
        #[command(subcommand)]
        command: PluginCommand,
    },

    /// Typed bus events — `shux events watch` long-polls, `shux events
    /// history` returns the ring buffer. Mirrors `events.*` RPC.
    Events {
        #[command(subcommand)]
        command: EventsCommand,
    },

    /// State mutations beyond single-entity ops (atomic batch, etc.).
    /// Mirrors `state.*` RPC.
    State {
        #[command(subcommand)]
        command: StateCommand,
    },

    /// Raw JSON-RPC fallthrough — `shux rpc call <method>` posts to
    /// the daemon and prints the structured `{result|error}` envelope.
    /// Use when a CLI wrapper doesn't exist yet for a method, or when
    /// scripting against newly-shipped RPC surface.
    Rpc {
        #[command(subcommand)]
        command: RpcCommand,
    },

    /// Print version information
    Version,

    /// Configuration helpers
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },

    /// Scaffold a `.shux/` directory in the current project.
    ///
    /// Creates `.shux/{templates,scripts,goldens,out}/` and `.shux/.gitignore`
    /// (gitignoring `out/`). Drops a starter `templates/review.toml` if no
    /// templates exist yet. Re-running is idempotent — never overwrites
    /// existing files.
    Init {
        /// Target directory (default: cwd).
        #[arg(short, long)]
        dir: Option<std::path::PathBuf>,
    },

    /// Internal: start the daemon (used by auto-start, not for users)
    #[command(name = "__daemon", hide = true)]
    #[allow(non_camel_case_types)]
    __daemon,
}

/// `shux state <verb>` — bulk state operations. Mirrors `state.*` RPC.
#[derive(Subcommand, Debug)]
pub enum StateCommand {
    /// Apply a declarative workspace template (TOML) atomically.
    ///
    /// Reads a session/windows/panes definition (PRD §10.3 shape),
    /// lowers it to a `state.apply` batch, and ships it to the daemon
    /// in one RPC. All graph mutations commit atomically; per-pane PTY
    /// spawn outcomes come back in the response. `--dry-run` validates
    /// + prints the planned ops without committing.
    Apply {
        /// Path to the TOML template (e.g. `./agent-conductor.toml`).
        template: std::path::PathBuf,

        /// Validate + print the lowered ops without sending the apply.
        #[arg(long)]
        dry_run: bool,

        /// After a successful apply, open `events watch` filtered to
        /// the new session and stream lifecycle events until Ctrl+C.
        #[arg(long)]
        watch: bool,
    },
}

/// `shux rpc call <method>` — raw JSON-RPC. Supports inline JSON,
/// `--params @<file>`, and `--params -` (stdin). Codex council May 2026
/// asked for these to eliminate shell-escaping bait on inline JSON.
#[derive(Subcommand, Debug)]
pub enum RpcCommand {
    /// Send one JSON-RPC request and print the structured response.
    Call {
        /// JSON-RPC method name (e.g., `session.create`, `window.list`).
        method: String,

        /// Params as one of: inline JSON (`'{"name":"work"}'`),
        /// `@<path>` (reads the file as JSON), or `-` (reads stdin
        /// as JSON). Defaults to `{}` for no-arg methods.
        #[arg(long, default_value = "{}", value_name = "JSON|@FILE|-")]
        params: String,
    },
}

/// Namespaced session verbs. Mirrors the `window`/`pane` subcommand
/// pattern and the `session.*` RPC namespace so agents that learned
/// the RPC method names can type them directly as CLI words.
#[derive(Subcommand, Debug)]
pub enum SessionCommand {
    /// Create a new session.
    Create {
        /// Session name as a positional argument. Equivalent to `-s NAME`.
        #[arg(value_name = "NAME")]
        name: Option<String>,

        /// Session name. Same field as the positional `NAME`.
        #[arg(short, long)]
        session: Option<String>,

        /// Create-if-missing semantics (maps to `session.ensure`).
        #[arg(long)]
        ensure: bool,

        /// Do not attach after creating the session.
        #[arg(short = 'd', long)]
        detached: bool,

        /// Working directory for the initial pane (default: current directory).
        #[arg(long, value_name = "DIR")]
        cwd: Option<PathBuf>,

        /// Manual title for the initial pane border.
        #[arg(long, value_name = "TITLE")]
        title: Option<String>,

        /// Shell command to run in the initial pane (single string).
        #[arg(long)]
        cmd: Option<String>,

        /// Trailing argv after `--` — exec'd directly (no shell wrapper).
        #[arg(last = true, num_args = 0..)]
        argv: Vec<String>,
    },

    /// List sessions.
    #[command(alias = "ls")]
    List {
        /// Reveal scratch sessions (lens PRD §8, LENS-R-041). Omitted by
        /// default; entries are flagged `scratch: true` when this is set.
        #[arg(long)]
        include_scratch: bool,
    },

    /// Kill a session.
    Kill {
        /// Session name OR UUID (positional or `-s/--session`; issue #88 —
        /// a UUID (e.g. the `session_id` a `lens run` response returns for
        /// a hidden scratch session) works here too, not just names.
        /// Precedence for UUID-shaped input: session ID first, falling back
        /// to a session NAMED that string; when both match, the ID wins
        /// (a warning is printed).
        #[arg(value_name = "NAME_OR_ID")]
        name_pos: Option<String>,

        #[arg(short, long, conflicts_with = "name_pos")]
        session: Option<String>,

        /// Optimistic concurrency on the session version.
        #[arg(long)]
        expected_version: Option<u64>,
    },

    /// Rename a session.
    Rename {
        /// Current session name.
        #[arg(short, long)]
        session: String,

        /// New name for the session.
        #[arg(short, long)]
        name: String,

        #[arg(long)]
        expected_version: Option<u64>,
    },

    /// Attach to an existing session.
    Attach {
        /// Session name (positional or `-s/--session`).
        #[arg(value_name = "NAME")]
        name_pos: Option<String>,

        #[arg(short, long, conflicts_with = "name_pos")]
        session: Option<String>,
    },

    /// Rasterize a session's active window to a composed PNG.
    /// Mirrors `session.snapshot` RPC. Equivalent to
    /// `shux window snapshot -s NAME` without `-w`, but namespaced
    /// under `session` per the "RPC dots become CLI spaces" invariant.
    Snapshot {
        /// Session name
        #[arg(short, long)]
        session: String,
        /// Output PNG path. If omitted, base64 is printed to stdout.
        #[arg(short, long)]
        output: Option<std::path::PathBuf>,
        /// Snapshot grid width in cells (4..=1000). Default: 120.
        #[arg(long, default_value_t = 120)]
        cols: u16,
        /// Snapshot grid height in cells (2..=1000). Default: 36.
        #[arg(long, default_value_t = 36)]
        rows: u16,
    },

    /// Save a live session as a reusable workspace template.
    Save {
        /// Session name.
        #[arg(short, long)]
        session: String,
        /// Output TOML path. If omitted, TOML is printed to stdout.
        #[arg(short, long)]
        output: Option<std::path::PathBuf>,
    },

    /// Restore a session from a saved workspace template.
    Restore {
        /// Saved TOML template path.
        template: std::path::PathBuf,
        /// Validate and print lowered ops without applying.
        #[arg(long)]
        dry_run: bool,
        /// Stream lifecycle events after restore.
        #[arg(long)]
        watch: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum ConfigCommand {
    /// Write a starter ~/.config/shux/config.toml + statusbar.toml.
    /// Refuses to overwrite by default; use --force to replace existing files.
    Init {
        /// Overwrite existing files.
        #[arg(short, long)]
        force: bool,
    },
    /// Print the current effective config path.
    Path,
    /// Print the canonical defaults (the same TOML you'd get from `init`).
    Show,
    /// Parse the user config (and every inline starship_config) and
    /// emit line:col diagnostics. Exit 0 = clean, 1 = at least one error.
    Validate {
        /// Path to validate (positional). Same as `--config`. Defaults
        /// to the user config path. Lets agent / CI flows validate a
        /// staged config without writing to `~/.config/shux/config.toml`.
        #[arg(value_name = "PATH", conflicts_with = "config")]
        path: Option<std::path::PathBuf>,

        /// Path to validate (flag form). Defaults to the user config path
        /// (`~/.config/shux/config.toml` or `$XDG_CONFIG_HOME/shux/config.toml`).
        #[arg(short, long)]
        config: Option<std::path::PathBuf>,
    },
    /// Reset onboarding state (welcome toast + prefix-discovery hint).
    /// Restores the first-launch experience. Useful for demos, recording
    /// walkthroughs, or just rediscovering the hint after running
    /// dogfood / iTerm tests that dismissed it.
    ResetHints,
}

#[derive(Subcommand, Debug)]
pub enum EventsCommand {
    /// Stream events from the daemon. Long-polls in a loop, printing one JSON
    /// Line per event to stdout. Suitable for piping into jq, grep, or an
    /// agent harness. Ctrl+C to stop.
    Watch {
        /// Filter event types by prefix (repeatable). Examples:
        /// `--filter pane.` matches all pane events; `--filter session.created`
        /// matches that exact event. Empty filter list means "all events".
        #[arg(short, long)]
        filter: Vec<String>,

        /// Resume from this sequence number. If omitted, starts at the current
        /// tail (next event published).
        #[arg(long)]
        from_seq: Option<u64>,

        /// Per-call long-poll timeout in ms (clamped 100..=30000). The CLI
        /// reissues the poll on timeout, so this only affects how often the
        /// daemon sees a fresh request.
        #[arg(long, default_value_t = 5000)]
        timeout_ms: u64,

        /// Stop after N events (useful for tests / scripted harnesses).
        #[arg(long)]
        limit: Option<u64>,
    },

    /// Print the last N events from the daemon's in-memory ring buffer
    /// (oldest → newest). Does NOT block.
    History {
        /// Filter event types by prefix (repeatable, same semantics as watch).
        #[arg(short, long)]
        filter: Vec<String>,

        /// Number of events to return (clamped 1..=1000).
        #[arg(short = 'n', long, default_value_t = 50)]
        count: u64,
    },
}

#[derive(Subcommand, Debug)]
pub enum PluginCommand {
    /// Scaffold a local Shux process plugin.
    Scaffold {
        /// Directory to create.
        path: std::path::PathBuf,

        /// Runtime template to generate.
        #[arg(long, value_enum, default_value_t = PluginScaffoldRuntime::Sh)]
        runtime: PluginScaffoldRuntime,

        /// Plugin name. Defaults to the directory basename.
        #[arg(long)]
        name: Option<String>,

        /// Stable plugin package id. Defaults to `local.shux.<name>`.
        #[arg(long)]
        id: Option<String>,

        /// Allow writing into a non-empty directory and replacing scaffold files.
        #[arg(long)]
        force: bool,
    },

    /// Alias for `plugin scaffold`.
    Create {
        /// Directory to create.
        path: std::path::PathBuf,

        /// Runtime template to generate.
        #[arg(long, value_enum, default_value_t = PluginScaffoldRuntime::Sh)]
        runtime: PluginScaffoldRuntime,

        /// Plugin name. Defaults to the directory basename.
        #[arg(long)]
        name: Option<String>,

        /// Stable plugin package id. Defaults to `local.shux.<name>`.
        #[arg(long)]
        id: Option<String>,

        /// Allow writing into a non-empty directory and replacing scaffold files.
        #[arg(long)]
        force: bool,
    },

    /// Scaffold a plugin in the current directory.
    Init {
        /// Runtime template to generate.
        #[arg(long, value_enum, default_value_t = PluginScaffoldRuntime::Sh)]
        runtime: PluginScaffoldRuntime,

        /// Plugin name. Defaults to the current directory basename.
        #[arg(long)]
        name: Option<String>,

        /// Stable plugin package id. Defaults to `local.shux.<name>`.
        #[arg(long)]
        id: Option<String>,

        /// Allow writing into a non-empty directory and replacing scaffold files.
        #[arg(long)]
        force: bool,
    },

    /// Spawn a plugin process, perform the JSON-RPC handshake, and
    /// register it under the name reported in its manifest. The
    /// executable must speak shux's line-delimited dialect — see
    /// `docs/tasks/044a-process-plugins-v0.md` and the
    /// `examples/plugins/hello/` reference plugin.
    Install {
        /// Path to the plugin executable.
        path: std::path::PathBuf,

        /// Extra argv passed to the plugin on spawn.
        #[arg(long, value_delimiter = ' ', num_args = 0..)]
        args: Vec<String>,

        /// Working directory for the plugin process.
        #[arg(long)]
        cwd: Option<std::path::PathBuf>,

        /// Disable hot reload. By default the daemon watches the
        /// plugin's source file and respawns it on every save
        /// (debounced ~250ms). Pass this to install the plugin
        /// without that watcher — useful for production / CI runs.
        #[arg(long)]
        no_watch: bool,
    },

    /// List running plugins (name, version, source, pid, status,
    /// uptime, declared subscriptions, watching).
    #[command(alias = "ls")]
    List,

    /// Send a plugin a `plugin.shutdown` notification, then terminate
    /// the child process after the grace window.
    Kill {
        /// Plugin name (as reported in its manifest).
        name: String,
    },

    /// Alias for graceful plugin shutdown/unregister.
    Stop {
        /// Plugin name (as reported in its manifest).
        name: String,
    },

    /// Manually kill+respawn a running plugin from the same source.
    /// Equivalent to a single hot-reload tick. Useful when a plugin
    /// was installed with `--no-watch` and you still want to bump it
    /// after editing the script.
    Reload {
        /// Plugin name (as reported in its manifest).
        name: String,
    },

    /// Grant a plugin authority to call a sensitive RPC method.
    /// See `docs/designs/permissions/README.md` for the model.
    ///
    /// Examples:
    ///   shux plugin grant conductor pane.snapshot
    ///   shux plugin grant conductor pane.send_keys --target a1b2c3d4-...
    ///   shux plugin grant watcher --subscribe pane.input.keystroke
    Grant {
        /// Plugin name.
        plugin: String,
        /// RPC method to grant (e.g. `pane.snapshot`), or — with
        /// `--subscribe` — an event filter to add to the manifest
        /// subscribes allow-set.
        method: String,
        /// Restrict the grant to a single target entity UUID. Without
        /// this flag the grant is blanket (`*`), covering any entity
        /// the method might be called against.
        #[arg(long)]
        target: Option<String>,
        /// Treat `method` as an event filter rather than an RPC
        /// method. Use this to widen the plugin's
        /// `manifest.subscribes` allow-set after hot reload — needed
        /// when the plugin author adds a new subscribe filter mid-
        /// session.
        #[arg(long)]
        subscribe: bool,
    },

    /// Revoke a previously-issued grant. Mirror of `grant`.
    Revoke {
        /// Plugin name.
        plugin: String,
        /// Method (or subscribe filter, with `--subscribe`) to remove.
        method: String,
        /// Single target UUID to drop from a target-scoped grant.
        /// Omit to drop the entire entry.
        #[arg(long)]
        target: Option<String>,
        /// Match `grant --subscribe` — operate on the subscribes
        /// allow-set rather than the grants table.
        #[arg(long)]
        subscribe: bool,
    },

    /// Show the grants for a plugin (method → scope, plus the
    /// manifest-subscribe allow-set).
    Grants {
        /// Plugin name.
        plugin: String,
    },

    /// Tail the per-plugin audit log (NDJSON, one entry per RPC
    /// frame). Reads
    /// `.shux/plugins/by-id/<uuid>/audit.log` for the plugin.
    Audit {
        /// Plugin name.
        plugin: String,
        /// Number of trailing lines to show (default 50, 0 = all).
        #[arg(long, short, default_value_t = 50)]
        tail: usize,
    },
}

#[derive(Subcommand, Debug)]
pub enum WindowCommand {
    /// List windows in a session
    #[command(alias = "ls")]
    List {
        /// Session name
        #[arg(short, long)]
        session: String,
    },

    /// Create a new window in a session. Mirrors `window.create` RPC.
    Create {
        /// Session name
        #[arg(short, long)]
        session: String,

        /// Window name (auto-generated if not provided)
        #[arg(short, long)]
        name: Option<String>,

        /// Working directory for the new window's initial pane.
        /// Defaults to the daemon's current working directory.
        #[arg(long)]
        cwd: Option<std::path::PathBuf>,

        /// Shell command to run in the new window's initial pane.
        /// Empty / omitted spawns the user's login+interactive shell.
        /// For exec-style passthrough use trailing `--` instead:
        /// `shux window create -s X -n W -- vim foo.rs`.
        #[arg(long)]
        cmd: Option<String>,

        /// Create-if-missing semantics (maps to window.ensure)
        #[arg(long)]
        ensure: bool,

        /// Trailing argv for the initial pane. Anything after `--`
        /// lands here and is exec'd directly (no shell wrapper).
        /// Takes precedence over `--cmd`.
        #[arg(last = true, num_args = 0..)]
        argv: Vec<String>,
    },

    /// Kill a window
    Kill {
        /// Session name
        #[arg(short, long)]
        session: String,

        /// Window name or index
        #[arg(short, long)]
        window: String,

        /// Optimistic concurrency: only succeed if the window is at
        /// this version. See `shux session kill --help` for details.
        #[arg(long)]
        expected_version: Option<u64>,
    },

    /// Rename a window
    Rename {
        /// Session name
        #[arg(short, long)]
        session: String,

        /// Current window name or index
        #[arg(short, long)]
        window: String,

        /// New window name
        #[arg(short, long)]
        name: String,

        /// Optimistic concurrency: only succeed if the window is at
        /// this version.
        #[arg(long)]
        expected_version: Option<u64>,
    },

    /// Focus (select) a window
    Focus {
        /// Session name
        #[arg(short, long)]
        session: String,

        /// Window name or index
        #[arg(short, long)]
        window: String,

        /// Optimistic concurrency: only succeed if the window is at
        /// this version.
        #[arg(long)]
        expected_version: Option<u64>,
    },

    /// Reorder (move) a window to a new index
    Reorder {
        /// Session name
        #[arg(short, long)]
        session: String,

        /// Window name or index
        #[arg(short, long)]
        window: String,

        /// New index position
        #[arg(short, long)]
        index: usize,

        /// Optimistic concurrency: only succeed if the window is at
        /// this version.
        #[arg(long)]
        expected_version: Option<u64>,
    },

    /// Rasterize a window's composed panes to a PNG. Mirrors `window.snapshot` RPC.
    ///
    /// Composes every pane in the target window — same picture you'd
    /// see in `shux session attach` — and rasterizes via shux-raster.
    /// Writes the PNG to `--output`, or prints base64 to stdout if
    /// omitted.
    Snapshot {
        /// Session to snapshot (defaults to the session's active window).
        #[arg(short, long)]
        session: Option<String>,
        /// Explicit window id or index. If omitted, the session's
        /// active window is used.
        #[arg(short, long)]
        window: Option<String>,
        /// Output PNG path. If omitted, base64 is printed to stdout.
        #[arg(short, long)]
        output: Option<std::path::PathBuf>,
        /// Snapshot grid width in cells (4..=1000). Default: 120.
        #[arg(long, default_value_t = 120)]
        cols: u16,
        /// Snapshot grid height in cells (2..=1000). Default: 36.
        #[arg(long, default_value_t = 36)]
        rows: u16,
    },
}

/// Parse a human duration into whole milliseconds (PRD §2.2: the CLI accepts
/// human durations and normalizes to ms for the RPC). Accepts a bare integer
/// (= milliseconds) or an integer with a `ms`/`s`/`m`/`h` suffix. A parse
/// error surfaces as a clap usage error → CLI exit 2. Used by
/// `pane wait-settled`'s `--quiet` / `--timeout`.
fn parse_duration_ms(s: &str) -> Result<u64, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty duration".to_string());
    }
    // `ms` MUST be checked before the single-char `s` suffix.
    let (digits, mult) = if let Some(n) = s.strip_suffix("ms") {
        (n, 1u64)
    } else if let Some(n) = s.strip_suffix('s') {
        (n, 1_000)
    } else if let Some(n) = s.strip_suffix('m') {
        (n, 60_000)
    } else if let Some(n) = s.strip_suffix('h') {
        (n, 3_600_000)
    } else {
        (s, 1) // bare integer == milliseconds
    };
    let value: u64 = digits
        .trim()
        .parse()
        .map_err(|_| format!("invalid duration {s:?} (use e.g. 300ms, 2s, 1m)"))?;
    value
        .checked_mul(mult)
        .ok_or_else(|| format!("duration {s:?} overflows"))
}

/// Parse a `pane glance --mask ROW,COL,WIDTH` redaction rect (task 080). All three are
/// `u16`; `WIDTH == 0` is rejected (a zero-width mask redacts nothing — likely a typo).
fn parse_mask_rect(s: &str) -> Result<(u16, u16, u16), String> {
    let parts: Vec<&str> = s.split(',').map(str::trim).collect();
    if parts.len() != 3 {
        return Err(format!("mask must be ROW,COL,WIDTH, got {s:?}"));
    }
    let field = |name: &str, v: &str| -> Result<u16, String> {
        v.parse::<u16>()
            .map_err(|_| format!("mask {name} {v:?} must be a u16"))
    };
    let row = field("ROW", parts[0])?;
    let col = field("COL", parts[1])?;
    let width = field("WIDTH", parts[2])?;
    if width == 0 {
        return Err("mask WIDTH must be > 0 (a zero-width mask redacts nothing)".to_string());
    }
    Ok((row, col, width))
}

#[derive(Subcommand, Debug)]
pub enum PaneCommand {
    /// List panes in a window
    #[command(alias = "ls")]
    List {
        /// Session name
        #[arg(short, long)]
        session: String,

        /// Window name or index (uses active window if not provided)
        #[arg(short, long)]
        window: Option<String>,
    },

    /// Split a pane
    Split {
        /// Session name
        #[arg(short, long)]
        session: String,

        /// Window name or index (uses active window if not provided)
        #[arg(short, long)]
        window: Option<String>,

        /// Pane UUID to split (uses active pane if not provided)
        #[arg(short, long)]
        pane: Option<String>,

        /// Split direction: vertical, horizontal, or auto
        #[arg(short, long)]
        direction: Option<String>,

        /// Split ratio (0.0-1.0, default 0.5)
        #[arg(short, long)]
        ratio: Option<f64>,
    },

    /// Focus a specific pane by UUID
    Focus {
        /// Session name
        #[arg(short, long)]
        session: String,

        /// Window name or index (uses active window if not provided)
        #[arg(short, long)]
        window: Option<String>,

        /// Pane UUID to focus
        #[arg(short, long)]
        pane: String,
    },

    /// Move focus in a direction (up/down/left/right)
    FocusDir {
        /// Session name
        #[arg(short, long)]
        session: String,

        /// Window name or index (uses active window if not provided)
        #[arg(short, long)]
        window: Option<String>,

        /// Direction: up, down, left, right
        #[arg(short, long)]
        direction: String,
    },

    /// Resize a pane
    Resize {
        /// Session name
        #[arg(short, long)]
        session: String,

        /// Window name or index (uses active window if not provided)
        #[arg(short, long)]
        window: Option<String>,

        /// Pane UUID to resize (uses active pane if not provided)
        #[arg(short, long)]
        pane: Option<String>,

        /// Resize direction: horizontal or vertical
        #[arg(short, long)]
        direction: String,

        /// Resize amount (0.0-1.0, default 0.1)
        #[arg(long)]
        delta: Option<f64>,

        /// Optimistic concurrency: only succeed if the pane is at
        /// this version. Layout ops (resize/zoom/swap) bump the version
        /// of every pane in the affected window.
        #[arg(long)]
        expected_version: Option<u64>,
    },

    /// Toggle zoom on a pane
    Zoom {
        /// Session name
        #[arg(short, long)]
        session: String,

        /// Window name or index (uses active window if not provided)
        #[arg(short, long)]
        window: Option<String>,

        /// Pane UUID to zoom (uses active pane if not provided)
        #[arg(short, long)]
        pane: Option<String>,

        /// Optimistic concurrency: only succeed if the pane is at
        /// this version.
        #[arg(long)]
        expected_version: Option<u64>,
    },

    /// Swap two panes
    Swap {
        /// Session name
        #[arg(short, long)]
        session: String,

        /// Window name or index (uses active window if not provided)
        #[arg(short, long)]
        window: Option<String>,

        /// First pane UUID
        #[arg(short, long)]
        pane: String,

        /// Second pane UUID (target to swap with)
        #[arg(short, long)]
        target: String,

        /// Optimistic concurrency: only succeed if pane (first) is at
        /// this version.
        #[arg(long)]
        expected_version: Option<u64>,
    },

    /// Kill a pane
    Kill {
        /// Session name
        #[arg(short, long)]
        session: String,

        /// Window name or index (uses active window if not provided)
        #[arg(short, long)]
        window: Option<String>,

        /// Pane UUID to kill
        #[arg(short, long)]
        pane: String,

        /// Optimistic concurrency: only succeed if the pane is at
        /// this version.
        #[arg(long)]
        expected_version: Option<u64>,
    },

    /// Set or clear a pane title (PR 4 / task 027).
    ///
    /// `shux pane title -s work -p <id> -t "build"` pins a manual
    /// title; `--clear` removes the manual override so OSC + command-
    /// derived auto-titles flow back into the border. `--no-auto`
    /// pins whatever is currently displayed and stops automatic
    /// re-derivation; `--auto` re-enables it.
    Title {
        /// Session name
        #[arg(short, long)]
        session: String,

        /// Window name or index (uses active window if not provided)
        #[arg(short, long)]
        window: Option<String>,

        /// Pane UUID (uses active pane if not provided)
        #[arg(short, long)]
        pane: Option<String>,

        /// New manual title. Conflicts with `--clear`.
        #[arg(short, long, conflicts_with = "clear")]
        title: Option<String>,

        /// Clear the manual title, letting OSC and command-derived
        /// auto-titles flow back through.
        #[arg(long)]
        clear: bool,

        /// Enable auto-title resolution (default state).
        #[arg(long, conflicts_with = "no_auto")]
        auto: bool,

        /// Disable auto-title resolution. Pins whatever is currently
        /// displayed.
        #[arg(long = "no-auto")]
        no_auto: bool,
    },

    /// Watch sampled PTY output from a pane (PR 2c).
    ///
    /// Long-polls `pane.output.watch` and prints each base64-decoded
    /// chunk to stdout. This is a low-overhead live observation stream,
    /// not a byte-exact transcript. Output is rate-limited at the source
    /// to ~10 chunks/sec/pane and may drop older bytes from a burst before
    /// publishing a sampled chunk. Absence-of-bytes assertions over this
    /// command are unsound; use `shux pane record --to FILE` for lossless
    /// capture.
    Watch {
        /// Session name (used to validate the pane belongs to a
        /// live session; the daemon also enforces this).
        #[arg(short, long)]
        session: String,

        /// Pane UUID to watch.
        #[arg(short, long)]
        pane: String,

        /// Per-poll long-poll timeout in ms (clamped 100..=30000).
        #[arg(long, default_value_t = 5000)]
        timeout_ms: u64,

        /// Stop after N chunks (useful for tests / scripted harnesses).
        /// Each chunk is one sample interval's worth of bytes.
        #[arg(long)]
        limit: Option<u64>,
    },

    /// Record lossless raw PTY output from a pane to a file.
    ///
    /// This tees bytes at the daemon's PTY read source before sampled
    /// `pane.output.watch` coalescing. It is byte-exact and intentionally
    /// applies backpressure if the destination cannot keep up. The start
    /// boundary is explicit: emit the stimulus you want audited only after
    /// this command has started recording.
    Record {
        /// Session name.
        #[arg(short, long)]
        session: String,

        /// Pane UUID to record.
        #[arg(short, long)]
        pane: String,

        /// Output file for raw PTY bytes.
        #[arg(long, value_name = "FILE")]
        to: std::path::PathBuf,

        /// Overwrite an existing output file.
        #[arg(long)]
        force: bool,

        /// Stop automatically after N milliseconds. Without this flag,
        /// recording continues until Ctrl-C.
        #[arg(long)]
        duration_ms: Option<u64>,
    },

    /// Send keystrokes to a pane
    SendKeys {
        /// Session name
        #[arg(short, long)]
        session: String,

        /// Window name or index (uses active window if not provided)
        #[arg(short, long)]
        window: Option<String>,

        /// Pane UUID (uses active pane if not provided)
        #[arg(short, long)]
        pane: Option<String>,

        /// Text to send (mutually exclusive with --data).
        ///
        /// `allow_hyphen_values` so agents can send literal flag-shaped
        /// strings (e.g. `--help`, `--version`) without resorting to
        /// `--text=--help` or base64 via `--data`.
        #[arg(short, long, allow_hyphen_values = true)]
        text: Option<String>,

        /// Base64-encoded bytes to send (mutually exclusive with --text)
        #[arg(long)]
        data: Option<String>,
    },

    /// Run a command in a pane and capture output
    Run {
        /// Session name
        #[arg(short, long)]
        session: String,

        /// Window name or index (uses active window if not provided)
        #[arg(short, long)]
        window: Option<String>,

        /// Pane UUID (uses active pane if not provided)
        #[arg(short, long)]
        pane: Option<String>,

        /// Command to run
        #[arg(short, long)]
        command: String,

        /// Timeout in seconds (default: 30)
        #[arg(long, default_value = "30")]
        timeout: u64,

        /// Run asynchronously (return command ID immediately)
        #[arg(long = "async")]
        is_async: bool,
    },

    /// Capture the current text content of a pane
    Capture {
        /// Session name
        #[arg(short, long)]
        session: String,

        /// Window name or index (uses active window if not provided)
        #[arg(short, long)]
        window: Option<String>,

        /// Pane UUID (uses active pane if not provided)
        #[arg(short, long)]
        pane: Option<String>,

        /// Number of lines to capture (default: 50)
        #[arg(short, long, default_value = "50")]
        lines: u64,
    },

    /// Rasterize a pane to a PNG. Mirrors `pane.snapshot` RPC.
    ///
    /// One pane only — for the composed multi-pane window image
    /// (with borders + titles + status bar) use `shux window snapshot`.
    ///
    /// Snapshot dimensions come from the pane's CURRENT size, not
    /// from flags here (`pane.snapshot` reads `vt.grid().cols/rows`).
    /// Use `shux pane set-size --cols N --rows M` first if you need
    /// the snapshot wider/taller.
    Snapshot {
        /// Session name
        #[arg(short, long)]
        session: String,
        /// Window name or index (uses active window if not provided)
        #[arg(short, long)]
        window: Option<String>,
        /// Pane UUID (uses active pane if not provided)
        #[arg(short, long)]
        pane: Option<String>,
        /// Output PNG path. If omitted, base64 is printed to stdout.
        #[arg(short, long)]
        output: Option<std::path::PathBuf>,
    },

    /// Atomic {png, text, revision} of one pane from ONE grid clone.
    /// Mirrors `pane.glance` RPC (lens PRD §5). Unlike `pane snapshot` +
    /// `pane capture` (two separate calls, two separate clones — can tear
    /// under concurrent writes), glance guarantees the PNG and text agree
    /// on the same frame.
    ///
    /// PNG bytes are never printed to stdout: use `--png <path>` to save
    /// them, or `--format json` for base64 inside the RPC result.
    Glance {
        /// Pane UUID.
        #[arg(value_name = "PANE")]
        pane: String,

        /// Write the rendered PNG to this path. Conflicts with
        /// `--text-only` (which disables PNG rendering entirely) — clap
        /// rejects the combination before any RPC round-trip (exit 2).
        #[arg(long, value_name = "PATH", conflicts_with = "text_only")]
        png: Option<std::path::PathBuf>,

        /// Skip PNG rendering entirely (`include_png=false`) — cheaper
        /// when only the text matters.
        #[arg(long)]
        text_only: bool,

        /// Render without the cursor overlay (`include_cursor=false`).
        #[arg(long)]
        no_cursor: bool,

        /// Store this glance as a checkpoint for a future `pane diff`
        /// (`checkpoint=true`).
        #[arg(long)]
        checkpoint: bool,

        /// Emit the canonical captured frame (`FrameEnvelope`, task-078 schema)
        /// as the `cells` field — the lens-gate `cell`-tier golden. Portable,
        /// JSON-only; no PNG is written for a cell golden (task 080).
        #[arg(long)]
        cells: bool,

        /// Write the canonical `cells` JSON to this path (implies `--cells`).
        /// Otherwise the envelope rides inside `--format json` output.
        #[arg(long, value_name = "PATH")]
        cells_out: Option<std::path::PathBuf>,

        /// Redact a rectangular region before serialize/hash/render, as
        /// `ROW,COL,WIDTH` (repeatable). Masks the emitted `cells`, `text`, AND
        /// PNG so a timestamp / token never enters a golden (task 080, D4).
        #[arg(long = "mask", value_name = "ROW,COL,WIDTH", value_parser = parse_mask_rect)]
        masks: Vec<(u16, u16, u16)>,
    },

    /// Block until a pane's screen has been STILL for a quiet window, or
    /// time out. Mirrors `pane.wait_settled` RPC (lens PRD §6). "Settled"
    /// means "quiet for --quiet", NOT "process finished": for slow-dripping
    /// output whose gaps exceed --quiet, pair this with `pane wait-for`
    /// (sentinel text). Exit 0 settled, exit 1 timeout.
    #[command(name = "wait-settled")]
    WaitSettled {
        /// Pane UUID (mirrors the RPC `pane_id`).
        #[arg(value_name = "PANE")]
        pane: String,

        /// Quiet window: settle once the pane has had this much silence.
        /// Human duration (`300ms`, `2s`); normalizes to ms. Range
        /// [10ms, 60s] — out of range → INVALID_PARAMS (exit 2).
        #[arg(long, default_value = "300ms", value_parser = parse_duration_ms)]
        quiet: u64,

        /// Overall deadline. Human duration (`10s`, `2s`); normalizes to
        /// ms. Range [quiet, 600s] — out of range → INVALID_PARAMS (exit 2).
        #[arg(long, default_value = "10s", value_parser = parse_duration_ms)]
        timeout: u64,
    },

    /// Capture the pane's current visible frame as a checkpoint for a later
    /// `pane diff`. Mirrors `pane.checkpoint` RPC (lens PRD §7). At most 4
    /// checkpoints per pane; a 5th evicts the oldest by creation revision
    /// (FIFO). Re-checkpointing the same revision is a no-op. Prints the
    /// keyed revision and any evicted revision.
    Checkpoint {
        /// Pane UUID (mirrors the RPC `pane_id`).
        #[arg(value_name = "PANE")]
        pane: String,
    },

    /// Diff the pane's current visible frame against a checkpointed revision.
    /// Mirrors `pane.diff_since` RPC (lens PRD §7). Prints the structured
    /// delta (changed cell count, per-row spans, changed row text). Exit 0 on
    /// any delta (diff is data, not a verdict); exit 5 on STALE_REVISION /
    /// RESIZE_INVALIDATED / PAYLOAD_TOO_LARGE (oversized heat PNG).
    Diff {
        /// Pane UUID (mirrors the RPC `pane_id`).
        #[arg(value_name = "PANE")]
        pane: String,

        /// The checkpointed revision to diff against (from `pane checkpoint`
        /// or a `--checkpoint` glance). Mirrors the RPC `since_revision`.
        #[arg(long, value_name = "REV")]
        since: u64,

        /// Write the heat PNG (changed cells overlaid, unchanged desaturated)
        /// to this path (`heat_png=true`).
        #[arg(long, value_name = "PATH")]
        heat: Option<std::path::PathBuf>,

        /// Skip the per-row changed text (`changed_row_text=false`).
        #[arg(long)]
        no_row_text: bool,
    },

    /// Resize a pane's PTY + VT grid to absolute (cols, rows).
    /// Mirrors `pane.set_size` RPC. Synchronous — the next snapshot
    /// sees the new dims. Use this BEFORE driving keystrokes when
    /// you need the pane wider/taller than the daemon default.
    #[command(name = "set-size")]
    SetSize {
        /// Session name
        #[arg(short, long)]
        session: String,
        /// Window name or index (uses active window if not provided)
        #[arg(short, long)]
        window: Option<String>,
        /// Pane UUID (uses active pane if not provided)
        #[arg(short, long)]
        pane: Option<String>,
        /// New width in cells (4..=1000).
        #[arg(long)]
        cols: u16,
        /// New height in cells (2..=1000).
        #[arg(long)]
        rows: u16,
    },

    /// Block until a pane's captured text matches (or stops matching)
    /// a needle. Mirrors `pane.wait_for` RPC. Replaces the iTerm2
    /// `wait_for_text` / `wait_for_absent` pattern across TUIs.
    ///
    /// Default targeting: with --session only, the wait runs against
    /// the session's *active pane* — typically the last-spawned pane
    /// in a multi-pane window. For multi-pane templates, pass an
    /// explicit `--pane <UUID>` (from `pane list` or `state.apply`'s
    /// spawn_results) so the wait targets the right pane.
    #[command(name = "wait-for")]
    WaitFor {
        /// Session id-or-name. Combined with --window / --pane to
        /// resolve a pane. With session alone, targets the active pane.
        #[arg(short, long)]
        session: Option<String>,
        /// Window id or index within the session.
        #[arg(short, long)]
        window: Option<String>,
        /// Explicit pane id (UUID). REQUIRED for multi-pane workspaces
        /// — the active pane is rarely the one you want to wait on.
        #[arg(short, long)]
        pane: Option<String>,
        /// Plain-text needle. The pane's last N lines (see --lines) are
        /// `contains()`-checked. Mutually exclusive with --regex.
        ///
        /// `allow_hyphen_values` is set because agents commonly wait
        /// for `--`-prefixed strings (CLI help output, flag names) and
        /// shouldn't have to know about the `--text=VAL` workaround.
        #[arg(short, long, conflicts_with = "regex", allow_hyphen_values = true)]
        text: Option<String>,
        /// Rust regex. Mutually exclusive with --text.
        #[arg(long, allow_hyphen_values = true)]
        regex: Option<String>,
        /// Wait for the needle to be ABSENT instead of present.
        #[arg(long)]
        absent: bool,
        /// How many recent lines to capture each poll. Default 200.
        #[arg(long, default_value_t = 200)]
        lines: u64,
        /// Total timeout in milliseconds. Default 10000, max 60000.
        #[arg(long, default_value_t = 10_000)]
        timeout_ms: u64,
        /// Poll interval in milliseconds. Default 100, range 20..=1000.
        #[arg(long, default_value_t = 100)]
        poll_ms: u64,
    },
}

/// §10 discoverability requirement: `shux lens` / `shux lens --help` prints
/// the five-verb loop recipe (naming the full `shux pane …` commands) so the
/// umbrella teaches the loop without duplicating commands under `lens`.
const LENS_LOOP_RECIPE: &str = "\
THE LENS LOOP (run \u{2192} settle \u{2192} glance \u{2192} drive \u{2192} diff):
  shux lens run -- <argv...>         spawn a command in a hidden scratch session
  shux pane wait-settled <pane>      block until the screen stops changing
  shux pane glance <pane>            atomic {png, text, revision} of one frame
  shux pane send-keys -s SID -p PANE -t ...   drive the pane (keystrokes)
  shux pane diff <pane> --since REV  prove exactly what changed, with PNG proof

`lens` is a CLI noun for exactly ONE verb (`run`) \u{2014} the other four verbs
above are pane primitives under `shux pane \u{2026}`, not `shux lens \u{2026}`.";

#[derive(Subcommand, Debug)]
#[command(after_help = LENS_LOOP_RECIPE)]
// A CLI arg enum parsed once per invocation, not stored hot — the `Gate` variant's rich
// 082 flag set makes it larger than `Run`, but boxing clap-derived fields buys nothing.
#[allow(clippy::large_enum_variant)]
pub enum LensCommand {
    /// Spawn `argv` directly (no shell, ever) in a hidden, quota-bounded
    /// scratch session. Mirrors `lens.run` RPC (lens PRD §8,
    /// LENS-R-040/045/046).
    ///
    /// Async by default: prints `{session_id, pane_id, revision}` and
    /// returns immediately. The scratch process keeps running; it is
    /// reaped `--ttl` after it exits, or at `--max-runtime` regardless of
    /// whether it has exited, whichever comes first (or immediately on an
    /// explicit `shux session kill`).
    ///
    /// `--wait` blocks the RPC until the command exits, adds `exit_code` to
    /// the printed output, and the CLI process itself exits with the
    /// CHILD's exit code once the child has started (§10 precedence rule —
    /// setup failures BEFORE the child starts use the table below instead).
    ///
    /// Signal death (killed by `--max-runtime`, an explicit `session kill`,
    /// or anything else that never lets the child report its own status
    /// code) has no POSIX exit code to report: the RPC's `exit_code` field
    /// comes back `-1`, and the CLI's process exit — like any Unix process
    /// exit — truncates to the low 8 bits, so the shell-visible `$?` is
    /// `255`, not `-1`. Treat 255 from `--wait` as "the process never
    /// exited on its own", not as a literal exit-code-255 from the child.
    Run {
        /// PTY size as `COLSxROWS` (e.g. `80x24`). Bounds cols in [20,500]
        /// rows in [5,200] are enforced server-side (INVALID_PARAMS, exit 2)
        /// — this flag only parses the shape, it does not pre-validate range.
        #[arg(long, value_name = "CxR", value_parser = parse_size_cxr, default_value = "80x24")]
        size: (u16, u16),

        /// How long to keep the scratch session around after the command
        /// exits, before reaping it. Human duration (`30s`, `1m`); range
        /// [0, 300s] enforced server-side.
        #[arg(long, value_parser = parse_duration_ms, default_value = "30s")]
        ttl: u64,

        /// Hard cap on the scratch session's total lifetime, regardless of
        /// whether the command has exited. Human duration (`1h`, `90s`);
        /// range [1s, 24h] enforced server-side.
        #[arg(long = "max-runtime", value_parser = parse_duration_ms, default_value = "1h")]
        max_runtime: u64,

        /// Extra environment variable for the spawned process,
        /// `KEY=VALUE`. Repeatable. Additions only — no inherit control
        /// in v1.
        #[arg(long = "env", value_name = "KEY=VALUE", value_parser = parse_env_kv)]
        env: Vec<(String, String)>,

        /// Working directory for the spawned process. Default: the
        /// daemon's cwd.
        #[arg(long, value_name = "PATH")]
        cwd: Option<PathBuf>,

        /// Block until the command exits; adds `exit_code` to the printed
        /// output and the CLI process exits with the child's code.
        #[arg(long)]
        wait: bool,

        /// Trailing argv after `--` — exec'd directly (no shell wrapper,
        /// ever; `argv[0]` is resolved via PATH). Required, non-empty.
        #[arg(last = true, num_args = 1.., required = true, value_name = "ARGV")]
        argv: Vec<String>,
    },

    /// Drive a declarative TOML scenario against a hidden scratch TUI and compare
    /// captured frames to committed goldens (task 081).
    ///
    /// The scenario file (`name`, `[terminal]`, `[env]`, `command`, `[[steps]]`)
    /// spawns `command` in a deterministic, deny-by-default sandbox (isolated
    /// HOME/XDG, `LC_ALL=C.UTF-8`, `TZ=UTC`, `TERM=xterm-256color`), then runs the
    /// agnostic step core (`wait_for_text`, `settle`, `type_text`, `keys`, `resize`,
    /// `expect_golden`, `assert_contains`, `expect_exit`, …). `expect_golden` settles
    /// the pane, captures the canonical frame, and compares it against
    /// `<scenario-dir>/goldens/<name>/` at the cell/pixel/exact tier.
    ///
    /// `expect_golden` settles the pane, captures the canonical frame, compares it against
    /// the committed golden at the cell/pixel/exact tier, and rolls the per-frame verdicts
    /// into a governed CI outcome: a machine-readable `report.json` (`--report`), an ASCII
    /// stdout summary, and a frozen exit-code contract (0 pass · 1 regression · 2 usage ·
    /// 3 infra · 5 child died · 6 update refused). A frame with no committed golden is a
    /// CI-safe regression (`missing_golden`) unless `--on-missing create`. `--update`
    /// re-blesses failing goldens (refused in CI / on a dirty tree / on a secret hit).
    #[command(args_conflicts_with_subcommands = true)]
    Gate {
        /// The scenario TOML file (required unless a `review`/`init` subcommand is used).
        #[arg(value_name = "SCENARIO")]
        scenario: Option<PathBuf>,

        /// Golden directory (default `<scenario-dir>/goldens/<scenario-name>/`).
        #[arg(long, value_name = "DIR")]
        golden_dir: Option<PathBuf>,

        /// Write the machine-readable `report.json` array to PATH, or `-` for stdout
        /// (stdout then carries ONLY the JSON; the summary moves to stderr).
        #[arg(long, value_name = "PATH|-")]
        report: Option<String>,

        /// First-run policy for a frame with no committed golden: `fail` (CI-safe →
        /// exit 1) or `create` (write a first golden locally; refused in CI).
        #[arg(long, value_enum, default_value_t = OnMissing::Fail)]
        on_missing: OnMissing,

        /// Re-bless goldens: `--update` (all failing frames) or `--update <name>` (one
        /// frame). Refused in CI, on a dirty golden tree, or on a pre-bless secret hit.
        #[arg(long, value_name = "failing|NAME", num_args = 0..=1, default_missing_value = "failing")]
        update: Option<String>,

        /// Reason recorded in `BASELINE-APPROVAL.md` when blessing.
        #[arg(long, value_name = "TEXT")]
        reason: Option<String>,

        /// Tolerance to record in a freshly-blessed golden sidecar as
        /// `MAX_CHANNEL_DELTA[,MAX_CHANGED_FRAC]` (bless-only; compare tol always comes
        /// from the blessed sidecar, never a runtime value).
        #[arg(long, value_name = "DELTA[,FRAC]", value_parser = parse_tol)]
        tol: Option<shux_vt::TolParams>,

        /// Directory for scratch evidence (heat PNGs). Default `.shux/out/<scenario>/`.
        #[arg(long, value_name = "DIR")]
        out: Option<PathBuf>,

        /// Retry budget for a flaky frame (parsed + carried into `report.json`; retry
        /// BEHAVIOUR lands in task 083).
        #[arg(long, value_name = "N")]
        retries: Option<u32>,

        /// Emit the raw runner-signal NDJSON trace to a path, or `-` for stdout.
        #[arg(long, value_name = "PATH|-")]
        trace: Option<String>,

        #[command(subcommand)]
        sub: Option<GateSubcommand>,

        /// Trailing argv after `--` overrides the scenario `command` (same argv,
        /// different binary — e.g. to point the scenario at a local build).
        #[arg(last = true, num_args = 0.., value_name = "ARGV")]
        argv: Vec<String>,
    },
}

/// First-run policy for a frame with no committed golden.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OnMissing {
    /// CI-safe: a missing golden is a regression (exit 1). The default.
    Fail,
    /// Write a first golden locally (through the approval-gated bless writer). Refused
    /// in CI so a golden can never be self-minted there.
    Create,
}

/// The `lens gate` sub-verbs beyond the default run (insta-style review + init).
#[derive(Debug, Subcommand)]
pub enum GateSubcommand {
    /// insta-style visual review: step through each changed frame and accept (bless),
    /// reject (leave failing), or skip. Renders before/after + heat inline where the
    /// terminal supports graphics, else writes PNGs to `--out` and prints paths.
    Review {
        /// The scenario TOML file.
        #[arg(value_name = "SCENARIO")]
        scenario: PathBuf,
        /// Golden directory (default `<scenario-dir>/goldens/<scenario-name>/`).
        #[arg(long, value_name = "DIR")]
        golden_dir: Option<PathBuf>,
        /// Directory for review PNGs. Default `.shux/out/<scenario>/`.
        #[arg(long, value_name = "DIR")]
        out: Option<PathBuf>,
    },
    /// Scaffold a new scenario `.toml` from a template and (approval-gated) write its
    /// first goldens. Refused in CI.
    Init {
        /// The scenario name (a safe single path component).
        #[arg(value_name = "NAME")]
        name: String,
        /// Directory to write the scenario `.toml` into. Default the current directory.
        #[arg(long, value_name = "DIR")]
        dir: Option<PathBuf>,
    },
}

/// Parse a bless tolerance `MAX_CHANNEL_DELTA[,MAX_CHANGED_FRAC]` (e.g. `8` or `8,0.01`).
fn parse_tol(s: &str) -> Result<shux_vt::TolParams, String> {
    let (delta, frac) = match s.split_once(',') {
        Some((d, f)) => (d.trim(), Some(f.trim())),
        None => (s.trim(), None),
    };
    let max_channel_delta: u16 = delta
        .parse()
        .map_err(|_| format!("invalid --tol delta {delta:?} (expected 0..=255)"))?;
    let max_changed_frac: f64 = match frac {
        Some(f) => f
            .parse()
            .map_err(|_| format!("invalid --tol frac {f:?} (expected 0.0..=1.0)"))?,
        None => 0.0,
    };
    if !(0.0..=1.0).contains(&max_changed_frac) {
        return Err(format!(
            "--tol frac {max_changed_frac} out of range 0.0..=1.0"
        ));
    }
    Ok(shux_vt::TolParams {
        max_channel_delta,
        max_changed_frac,
    })
}

/// Parse a `COLSxROWS` size flag (e.g. `80x24`) into `(cols, rows)`. Shape
/// only — range bounds are an RPC-level INVALID_PARAMS, not a clap usage
/// error (matches the settle `--quiet`/`--timeout` convention: the CLI
/// normalizes shape, the server owns the range contract).
fn parse_size_cxr(s: &str) -> Result<(u16, u16), String> {
    let (cols, rows) = s
        .split_once('x')
        .or_else(|| s.split_once('X'))
        .ok_or_else(|| format!("invalid size {s:?} (expected COLSxROWS, e.g. 80x24)"))?;
    let cols: u16 = cols
        .trim()
        .parse()
        .map_err(|_| format!("invalid size {s:?}: {cols:?} is not a valid column count"))?;
    let rows: u16 = rows
        .trim()
        .parse()
        .map_err(|_| format!("invalid size {s:?}: {rows:?} is not a valid row count"))?;
    Ok((cols, rows))
}

/// Parse a `KEY=VALUE` env flag into a `(String, String)` pair.
fn parse_env_kv(s: &str) -> Result<(String, String), String> {
    let (k, v) = s
        .split_once('=')
        .ok_or_else(|| format!("invalid env entry {s:?} (expected KEY=VALUE)"))?;
    if k.is_empty() {
        return Err(format!("invalid env entry {s:?}: empty key"));
    }
    Ok((k.to_string(), v.to_string()))
}

impl Cli {
    /// Determine the socket path to use. Priority:
    /// 1. Explicit --socket flag / SHUX_SOCKET env (handled by clap env)
    /// 2. $XDG_RUNTIME_DIR/shux/shux.sock
    /// 3. /tmp/shux-$UID/shux.sock
    pub fn socket_path(&self) -> PathBuf {
        if let Some(ref path) = self.socket {
            return path.clone();
        }
        crate::daemon::socket_path().unwrap_or_else(|_| PathBuf::from("/tmp/shux/shux.sock"))
    }
}

/// Format an RPC error for human display. Dispatch on the JSON-RPC
/// CODE first — `version_conflict` (-32002) carries the same
/// `id`+`resource` envelope as `not_found` (-32004), so a
/// presence-of-fields heuristic mis-reports concurrency conflicts as
/// "not found" (issue #25 §3).
fn rpc_display(code: i64, message: &str, data: Option<&serde_json::Value>) -> String {
    let resource = data
        .and_then(|d| d.get("resource"))
        .and_then(|v| v.as_str())
        .unwrap_or("resource");
    let id_field = data.and_then(|d| d.get("id")).and_then(|v| v.as_str());

    match code {
        // not_found
        -32004 => match id_field {
            Some(id) => format!("{resource} '{id}' not found"),
            None => format!("{resource} not found"),
        },
        // version_conflict
        -32002 => {
            let expected = data
                .and_then(|d| d.get("expected_version"))
                .and_then(|v| v.as_u64());
            let actual = data
                .and_then(|d| d.get("actual_version"))
                .and_then(|v| v.as_u64());
            match (id_field, expected, actual) {
                (Some(id), Some(e), Some(a)) => format!(
                    "{resource} '{id}' version_conflict: expected {e}, actual {a} \
                     (re-read state and retry with the current version)"
                ),
                _ => format!("{resource} version_conflict — re-read state and retry"),
            }
        }
        // name_conflict — `data.name` carries the colliding name
        -32003 => {
            if let Some(name) = data.and_then(|d| d.get("name")).and_then(|v| v.as_str()) {
                format!("{resource} name '{name}' already exists")
            } else {
                format!("{resource} name_conflict")
            }
        }
        // invalid_params / internal — use `detail` when present
        _ => {
            if let Some(detail) = data.and_then(|d| d.get("detail")).and_then(|v| v.as_str()) {
                return detail.to_string();
            }
            format!("RPC error {code}: {message}")
        }
    }
}

/// Errors that can occur during RPC communication.
#[derive(Debug, thiserror::Error)]
pub enum RpcClientError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("response frame too large: {0} bytes (max 16 MB)")]
    FrameTooLarge(usize),
    #[error("{}", rpc_display(*.code, message, data.as_ref()))]
    Rpc {
        code: i64,
        message: String,
        data: Option<serde_json::Value>,
    },
}

/// Send a JSON-RPC request over a UDS and read the response.
/// Uses 4-byte big-endian length-prefix framing (matching server in task 008).
pub async fn rpc_call(
    stream: &mut tokio::net::UnixStream,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, RpcClientError> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": uuid::Uuid::new_v4().to_string(),
        "method": method,
        "params": params,
    });

    let payload = serde_json::to_vec(&request)?;

    // Write length prefix (4 bytes, big-endian)
    let len = payload.len() as u32;
    stream.write_all(&len.to_be_bytes()).await?;
    stream.write_all(&payload).await?;
    stream.flush().await?;

    // Read response length prefix
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await?;
    let resp_len = u32::from_be_bytes(len_buf) as usize;

    // Enforce max frame size (16 MB per PRD §8.1)
    if resp_len > 16 * 1024 * 1024 {
        return Err(RpcClientError::FrameTooLarge(resp_len));
    }

    // Read response payload
    let mut resp_buf = vec![0u8; resp_len];
    stream.read_exact(&mut resp_buf).await?;

    let response: serde_json::Value = serde_json::from_slice(&resp_buf)?;

    if let Some(error) = response.get("error") {
        let code = error.get("code").and_then(|v| v.as_i64()).unwrap_or(0);
        let message = error
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown error")
            .to_string();
        let data = error.get("data").cloned();
        return Err(RpcClientError::Rpc {
            code,
            message,
            data,
        });
    }

    Ok(response
        .get("result")
        .cloned()
        .unwrap_or(serde_json::Value::Null))
}

/// Convert CLI OutputFormat to style OutputFormat.
fn to_style_format(format: OutputFormat) -> crate::style::OutputFormat {
    match format {
        OutputFormat::Text => crate::style::OutputFormat::Text,
        OutputFormat::Json => crate::style::OutputFormat::Json,
        OutputFormat::Plain => crate::style::OutputFormat::Plain,
    }
}

/// Format a created_at timestamp as relative time.
fn format_created_at(value: &serde_json::Value) -> String {
    value
        .as_str()
        .map(String::from)
        .or_else(|| {
            value.as_u64().map(|ts| {
                let dt = std::time::UNIX_EPOCH + std::time::Duration::from_secs(ts);
                let elapsed = dt.elapsed().unwrap_or_default();
                if elapsed.as_secs() < 60 {
                    format!("{}s ago", elapsed.as_secs())
                } else if elapsed.as_secs() < 3600 {
                    format!("{}m ago", elapsed.as_secs() / 60)
                } else {
                    format!("{}h ago", elapsed.as_secs() / 3600)
                }
            })
        })
        .unwrap_or_else(|| "?".to_string())
}

/// Handle the `shux session list` command.
pub async fn handle_ls(
    stream: &mut tokio::net::UnixStream,
    include_scratch: bool,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let result = rpc_call(
        stream,
        "session.list",
        serde_json::json!({ "include_scratch": include_scratch }),
    )
    .await?;

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        OutputFormat::Text | OutputFormat::Plain => {
            use crate::style;

            let ctx = style::TerminalContext::detect(to_style_format(format));

            let sessions = result
                .get("sessions")
                .and_then(|v| v.as_array())
                .or_else(|| result.as_array());

            let session_infos: Vec<style::SessionInfo> = sessions
                .map(|arr| {
                    arr.iter()
                        .map(|s| {
                            let name = s
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("(unnamed)")
                                .to_string();
                            let id = s
                                .get("id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("?")
                                .to_string();
                            let window_count = s
                                .get("windows")
                                .and_then(|v| v.as_array())
                                .map(|a| a.len())
                                .or_else(|| {
                                    s.get("window_count")
                                        .and_then(|v| v.as_u64())
                                        .map(|n| n as usize)
                                })
                                .unwrap_or(0);
                            let created = s
                                .get("created_at")
                                .map(format_created_at)
                                .unwrap_or_else(|| "?".to_string());
                            let scratch =
                                s.get("scratch").and_then(|v| v.as_bool()).unwrap_or(false);
                            style::SessionInfo {
                                name,
                                id,
                                window_count,
                                created,
                                is_active: false, // no attach tracking yet
                                scratch,
                            }
                        })
                        .collect()
                })
                .unwrap_or_default();

            style::render_session_list(&ctx, &session_infos);
        }
    }

    Ok(())
}

/// Default contents written by `shux config init`. ONE file. The
/// `[[statusbar.segment]]` entries embed their starship config inline
/// via `starship_config = """..."""` — no separate `statusbar.toml`
/// to maintain. `shux config show` returns the same bytes.
pub const DEFAULT_CONFIG_TOML: &str = r##"# ~/.config/shux/config.toml
#
# shux user configuration. The daemon hot-reloads this file: edits land
# in attached sessions on the next render frame, no restart needed.

[appearance]
# Pane border style: thin | thick | double | rounded | ascii | none
border_style = "rounded"
# Render the status bar with Nerd Font glyphs (terminal icon, git
# branch, window icon, ssh host). Default true — shux bundles the
# full JetBrains Mono Nerd Font (2.4 MB) so the PNG rasterizer
# resolves every NF codepoint OOTB, no tofu. In a live attach, your
# terminal's font decides; set to false here if your terminal lacks
# NF — the ASCII fallback (◆ ± ▶ @) works in any font.
nerd_fonts = true
# Optional custom primary text font for the PNG rasterizer. The
# bundled NF JetBrains Mono, text-symbol fallbacks, and Noto Emoji
# stay in the fallback chain so common glyphs your font lacks
# (typical for plain non-patched typefaces, TUI symbols, or standalone
# emoji) still resolve — no tofu either way. Doesn't affect live
# attach (your terminal font controls that).
# Font changes hot-reload: edit this line and the next snapshot uses
# the new font. On a bad path the last-good rasterizer is retained
# and a warning is logged.
# font = "/path/to/your-font.ttf"
#
# Optional ordered fallback chain for PNG snapshots only. Entries can
# be builtin tokens or absolute font paths. Omit this field to use the
# default builtin chain shown here. Set it explicitly when a TUI needs
# a local/system font without changing the primary metrics font.
# Empty lists are invalid. If font is unset, bundled JetBrains Mono
# remains the primary metrics font and this list only changes glyph
# fallback coverage.
# font_fallbacks = ["builtin:nerd-font", "builtin:math", "builtin:symbols", "builtin:symbols-legacy", "builtin:emoji"]
#
# For status-bar segments, paste either the literal glyph (UTF-8) into
# a single-quoted TOML string, or use TOML's escape inside a
# double-quoted string. Note TOML uses bare \uXXXX (4-hex BMP) or
# \UXXXXXXXX (8-hex, supplementary plane) — NOT Rust's \u{XXXX} form:
#   nf-pl-branch      U+E0A0   ''  or  "\uE0A0"
#   nf-md-kubernetes  U+F10FE  '󱃾'  or  "\U000F10FE"
#   nf-md-ship_wheel  U+F124A  '󱉊'  or  "\U000F124A"
# Common text UI glyphs (↻, ⠹, ✔, ✗, ⏎, ⌥) and standalone
# monochrome emoji (🍺 🧩 🦀 🚀 ⚡ …) render correctly in PNG snapshots
# via bundled fallbacks — no extra configuration needed. Colour emoji
# and composed emoji (ZWJ sequences like 👨‍💻, VS16 like 🛠️,
# regional-indicator flag pairs, skin-tone modifiers) are not yet
# supported — the parser splits them into separate cells. For composed
# glyphs in status bars, configure your starship language modules with
# the NF equivalent.
# Example for rust: symbol = ' ' (or
# symbol = "\uE7A8 " using TOML escape syntax).

[keys]
# Prefix key (e.g. "ctrl-space", "ctrl-b", "alt-w")
prefix = "ctrl-space"

[keybindings]
# Optional attach key overrides. Keys use the same notation as prefix:
# "alt-h" targets the root table; "prefix h" targets the key pressed
# after the configured prefix. Values are action names, for example:
#   "alt-h" = "focus-left"
#   "prefix c" = "new-window"
#   "prefix [" = "copy-mode"

# ─────────────────────────────────────────────────────────────────────
# Theme: override the built-in Catppuccin Macchiato palette. Every key
# is optional; missing keys fall through to the defaults so an empty
# (or absent) [theme] block is equivalent to no [theme] at all. Edits
# hot-reload like the rest of the file — borders + status bar pick up
# the new colors on the next render frame.
# ─────────────────────────────────────────────────────────────────────

# [theme]
# border_focused   = "#74c7ec"   # Catppuccin Sapphire (default)
# border_unfocused = "#5b6078"   # Catppuccin Surface2 (default)
# status_bg        = "#1e2030"   # Catppuccin Crust
# status_fg        = "#cad3f5"   # Catppuccin Text
# status_accent    = "#74c7ec"   # Catppuccin Sapphire

# ─────────────────────────────────────────────────────────────────────
# Status-bar segments. Each entry runs `command` every `interval_ms`
# and renders the captured stdout (ANSI colors preserved) into the
# named zone. Fallback text shows when the command is missing or
# fails — keeps the bar pretty even on machines without the binary.
#
# `starship_config` is an INLINE TOML string. shux materialises it to
# a tempfile per segment and exports `STARSHIP_CONFIG=<tempfile>` for
# the spawned `starship prompt` invocation. The runner also defaults
# Starship status-bar spawns to raw ANSI output (`STARSHIP_SHELL=cmd`,
# `TERM=xterm-256color`) so shell prompt guards like Bash `\[` / `\]`
# never leak into the bar. Your shell PS1 (driven by
# `~/.config/starship.toml`) is unaffected — only the segment spawn
# sees these overrides.
# ─────────────────────────────────────────────────────────────────────

[[statusbar.segment]]
zone = "right"
command = ["starship", "prompt"]
interval_ms = 1000
fallback = " (starship not installed) "
env = { STARSHIP_SHELL = "cmd", TERM = "xterm-256color" }
starship_config = """
add_newline = false
format = '''
$git_branch\
$git_status\
$rust\
$python\
$nodejs\
$cmd_duration\
$time\
'''

[time]
disabled = false
format = ' [$time](bold #f5a97f) '
time_format = '%H:%M'

[git_branch]
format = '[$symbol$branch]($style) '
style = 'bold #c6a0f6'
# nf-pl-branch (U+E0A0).
symbol = " "

[git_status]
format = '[$all_status$ahead_behind]($style)'
style = 'bold #ed8796'

[rust]
format = '[$symbol($version)]($style) '
style = 'bold #ee99a0'
# nf-dev-rust (U+E7A8).
symbol = " "

[python]
format = '[$symbol${pyenv_prefix}(${version} )(($virtualenv) )]($style)'
style = 'bold #eed49f'
# nf-dev-python (U+E73C).
symbol = " "

[nodejs]
format = '[$symbol($version)]($style) '
style = 'bold #a6da95'
# nf-dev-nodejs (U+E718).
symbol = " "

[cmd_duration]
min_time = 0
format = '[ $duration]($style) '
style = 'bold #91d7e3'
"""
"##;

pub const SHELL_HINT: &str = r##"
SUGGESTED ~/.bashrc / ~/.zshrc snippet:

  # Skip the rich starship PS1 when shux is hosting (the status bar has it).
  if command -v starship >/dev/null 2>&1; then
    if [[ -n $SHUX ]]; then
      PS1='\[\e[36m\]❯\[\e[0m\] '
    else
      eval "$(starship init bash)"
    fi
  fi

This makes the in-pane prompt a clean cyan chevron, while the status
bar at the bottom of the screen carries the rich starship segments.
"##;

/// `shux config init`: scaffold a single ~/.config/shux/config.toml
/// with an inline starship status-bar config. No second file.
pub fn handle_config_init(force: bool) -> anyhow::Result<()> {
    use crate::style;

    let cfg_path = shux_core::config::default_config_path();
    if let Some(parent) = cfg_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    write_or_skip(&cfg_path, DEFAULT_CONFIG_TOML, force)?;

    style::print_success(
        "Config initialised at",
        cfg_path.display().to_string().as_str(),
        None,
    );
    println!("{}", SHELL_HINT);
    Ok(())
}

fn write_or_skip(path: &std::path::Path, contents: &str, force: bool) -> anyhow::Result<()> {
    if path.exists() && !force {
        eprintln!(
            "skip {} (exists; pass --force to overwrite)",
            path.display()
        );
        return Ok(());
    }
    std::fs::write(path, contents)?;
    Ok(())
}

pub fn handle_config_path() -> anyhow::Result<()> {
    let p = shux_core::config::default_config_path();
    println!("{}", p.display());
    Ok(())
}

/// `shux config reset-hints` — wipe the onboarding state file so the
/// next attach shows the welcome toast and right-zone hint again.
/// Idempotent: silently succeeds if the file isn't there.
pub fn handle_config_reset_hints() -> anyhow::Result<()> {
    let path = onboarding_state_path();
    let existed = path.exists();
    if existed {
        std::fs::remove_file(&path)?;
    }
    if existed {
        crate::style::print_success(
            "Reset onboarding hints",
            path.display().to_string().as_str(),
            None,
        );
    } else {
        crate::style::print_success(
            "Onboarding state already clear",
            path.display().to_string().as_str(),
            None,
        );
    }
    println!(
        "  {} the welcome toast and right-zone hint will show again on the next `shux` attach.",
        crate::style::muted("→")
    );
    Ok(())
}

/// Where onboarding.json lives. Matches `onboarding::state_file_path`
/// but lives here too because the cli layer can't import the bin-only
/// `onboarding` module (Rust's privacy rules mean the path logic is
/// duplicated, but it's 5 lines and changes never).
fn onboarding_state_path() -> std::path::PathBuf {
    if let Ok(xdg) = std::env::var("XDG_STATE_HOME") {
        return std::path::PathBuf::from(xdg)
            .join("shux")
            .join("onboarding.json");
    }
    if let Ok(home) = std::env::var("HOME") {
        return std::path::PathBuf::from(home)
            .join(".local")
            .join("state")
            .join("shux")
            .join("onboarding.json");
    }
    std::path::PathBuf::from("onboarding.json")
}

pub fn handle_config_show() -> anyhow::Result<()> {
    print!("{}", DEFAULT_CONFIG_TOML);
    Ok(())
}

/// `shux config validate [PATH | --config <path>]`. Returns the process
/// exit code that the caller should propagate (0 clean, 1 had diagnostics).
pub fn handle_config_validate(path: Option<std::path::PathBuf>) -> anyhow::Result<i32> {
    let resolved = path.unwrap_or_else(shux_core::config::default_config_path);
    let used_default = resolved == shux_core::config::default_config_path();

    if !resolved.exists() {
        if used_default {
            crate::style::print_error(&format!(
                "config file not found: {} — run `shux config init` to scaffold one, \
                 or pass a path: `shux config validate <PATH>`",
                resolved.display()
            ));
        } else {
            crate::style::print_error(&format!("config file not found: {}", resolved.display()));
        }
        return Ok(1);
    }

    let diags = crate::config_validate::validate(&resolved)?;
    Ok(crate::config_validate::print_diagnostics(&diags, &resolved))
}

/// Handle the `shux session create` command.
#[derive(Debug)]
pub struct SessionCreateOptions {
    pub session_name: Option<String>,
    pub cwd: Option<std::path::PathBuf>,
    pub title: Option<String>,
    pub cmd: Option<String>,
    pub argv: Vec<String>,
    pub ensure: bool,
}

pub async fn handle_new(
    stream: &mut tokio::net::UnixStream,
    opts: SessionCreateOptions,
    format: OutputFormat,
) -> anyhow::Result<serde_json::Value> {
    let invocation_cwd = std::env::current_dir()
        .map_err(|e| anyhow::anyhow!("failed to determine current directory: {e}"))?;
    let cwd = resolve_session_create_cwd(opts.cwd, &invocation_cwd);
    let params =
        build_session_create_params(opts.session_name, cwd, opts.title, opts.cmd, opts.argv);

    let method = if opts.ensure {
        "session.ensure"
    } else {
        "session.create"
    };
    let result = rpc_call(stream, method, serde_json::Value::Object(params)).await?;

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        OutputFormat::Text | OutputFormat::Plain => {
            use crate::style;

            let name = result
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("(unnamed)");
            let id = result.get("id").and_then(|v| v.as_str()).unwrap_or("?");
            style::print_session_created(name, id, opts.ensure);
        }
    }

    Ok(result)
}

fn resolve_session_create_cwd(
    cwd: Option<std::path::PathBuf>,
    invocation_cwd: &std::path::Path,
) -> std::path::PathBuf {
    let cwd = cwd.unwrap_or_else(|| invocation_cwd.to_path_buf());
    if cwd.is_absolute() {
        cwd
    } else {
        invocation_cwd.join(cwd)
    }
}

fn build_session_create_params(
    session_name: Option<String>,
    cwd: std::path::PathBuf,
    title: Option<String>,
    cmd: Option<String>,
    argv: Vec<String>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut params = serde_json::Map::new();
    if let Some(name) = session_name {
        params.insert("name".to_string(), serde_json::Value::String(name));
    }
    params.insert(
        "cwd".to_string(),
        serde_json::Value::String(cwd.display().to_string()),
    );
    if let Some(title) = title {
        params.insert("pane_title".to_string(), serde_json::Value::String(title));
    }
    // argv (trailing `--`) wins over --cmd if both are given.
    if !argv.is_empty() {
        params.insert(
            "command".to_string(),
            serde_json::Value::Array(argv.into_iter().map(serde_json::Value::String).collect()),
        );
    } else if let Some(command) = cmd {
        params.insert("command".to_string(), serde_json::Value::String(command));
    }
    params
}

/// Handle the `shux session kill` command.
///
/// Accepts either a session NAME or a session UUID (issue #88 direction):
/// `lens.run` returns `session_id` as a UUID, and scratch sessions are
/// excluded from the default `session.list` a name lookup would need. A
/// UUID-shaped argument resolves as an id FIRST with fallback to name
/// lookup (session names may legally be UUID-shaped; id wins when both
/// match — see `resolve_uuid_shaped_session`), then goes out as the RPC's
/// `id` param, which `session.kill` has always accepted. Plain names go
/// out as `name`, unchanged.
pub async fn handle_kill(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    expected_version: Option<u64>,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let mut params = serde_json::Map::new();
    let (key, value) = if let Ok(parsed) = uuid::Uuid::parse_str(session_name) {
        (
            "id",
            resolve_uuid_shaped_session(stream, session_name, parsed).await?,
        )
    } else {
        ("name", session_name.to_string())
    };
    params.insert(key.to_string(), serde_json::Value::String(value));
    if let Some(ev) = expected_version {
        params.insert("expected_version".to_string(), serde_json::Value::from(ev));
    }

    let result = rpc_call(stream, "session.kill", serde_json::Value::Object(params)).await?;

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        OutputFormat::Text | OutputFormat::Plain => {
            crate::style::print_session_killed(session_name);
        }
    }

    Ok(())
}

/// Handle the `shux session rename` command.
pub async fn handle_rename(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    new_name: &str,
    expected_version: Option<u64>,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let mut params = serde_json::Map::new();
    params.insert(
        "name".to_string(),
        serde_json::Value::String(session_name.to_string()),
    );
    params.insert(
        "new_name".to_string(),
        serde_json::Value::String(new_name.to_string()),
    );
    if let Some(ev) = expected_version {
        params.insert("expected_version".to_string(), serde_json::Value::from(ev));
    }

    let result = rpc_call(stream, "session.rename", serde_json::Value::Object(params)).await?;

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        OutputFormat::Text | OutputFormat::Plain => {
            crate::style::print_session_renamed(session_name, new_name);
        }
    }

    Ok(())
}

/// Handle the `shux session save` command.
pub async fn handle_session_save(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    output: Option<std::path::PathBuf>,
) -> anyhow::Result<()> {
    let result = rpc_call(
        stream,
        "session.export_template",
        serde_json::json!({ "name": session_name }),
    )
    .await?;
    let template = result
        .get("template")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("session.export_template returned no template"))?;

    if let Some(path) = output {
        std::fs::write(&path, template)?;
        crate::style::print_success("saved", &path.display().to_string(), None);
    } else {
        print!("{template}");
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn handle_wait_for(
    stream: &mut tokio::net::UnixStream,
    session: Option<&str>,
    window: Option<&str>,
    pane: Option<&str>,
    text: Option<&str>,
    regex: Option<&str>,
    absent: bool,
    lines: u64,
    timeout_ms: u64,
    poll_ms: u64,
    format: OutputFormat,
) -> anyhow::Result<()> {
    if text.is_none() && regex.is_none() {
        anyhow::bail!("provide --text or --regex");
    }

    let mut params = serde_json::Map::new();
    if let Some(p) = pane {
        params.insert("pane_id".into(), serde_json::Value::String(p.to_string()));
    } else if let Some(s) = session {
        let sid = resolve_session_id(stream, s).await?;
        params.insert("session_id".into(), serde_json::Value::String(sid.clone()));
        if let Some(w) = window {
            let (wid, _t) = resolve_window_id(stream, &sid, w).await?;
            params.insert("window_id".into(), serde_json::Value::String(wid));
        }
    } else {
        anyhow::bail!("provide --pane or --session [--window]");
    }
    if let Some(t) = text {
        params.insert("text".into(), serde_json::Value::String(t.to_string()));
    }
    if let Some(r) = regex {
        params.insert("regex".into(), serde_json::Value::String(r.to_string()));
    }
    params.insert("absent".into(), serde_json::Value::Bool(absent));
    params.insert("lines".into(), serde_json::Value::from(lines));
    params.insert("timeout_ms".into(), serde_json::Value::from(timeout_ms));
    params.insert("poll_ms".into(), serde_json::Value::from(poll_ms));

    let result = match rpc_call(stream, "pane.wait_for", serde_json::Value::Object(params)).await {
        Ok(v) => v,
        Err(RpcClientError::Rpc {
            code,
            message,
            data,
        }) => {
            match format {
                OutputFormat::Json => {
                    let env = serde_json::json!({
                        "error": { "code": code, "message": message, "data": data }
                    });
                    println!("{}", serde_json::to_string_pretty(&env)?);
                }
                _ => {
                    eprintln!("{} {message}", crate::style::error("✗ wait-for:"));
                    if let Some(d) = data
                        .as_ref()
                        .and_then(|v| v.get("last_capture_preview"))
                        .and_then(|v| v.as_str())
                    {
                        eprintln!("{}", crate::style::muted("  last captured:"));
                        for line in d.lines().take(8) {
                            eprintln!("    {line}");
                        }
                    }
                }
            }
            std::process::exit(2);
        }
        Err(e) => return Err(e.into()),
    };

    match format {
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&result)?),
        _ => {
            let elapsed = result
                .get("elapsed_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let abs = if absent { " (absent)" } else { "" };
            println!(
                "{} matched after {}ms{abs}",
                crate::style::success("✓ wait-for"),
                elapsed,
            );
        }
    }

    Ok(())
}

pub async fn handle_snapshot(
    stream: &mut tokio::net::UnixStream,
    session: Option<&str>,
    window: Option<&str>,
    output: Option<std::path::PathBuf>,
    cols: u16,
    rows: u16,
    format: OutputFormat,
) -> anyhow::Result<()> {
    use base64::Engine;

    let mut params = serde_json::Map::new();
    params.insert("cols".into(), serde_json::Value::from(cols));
    params.insert("rows".into(), serde_json::Value::from(rows));

    let method = match (session, window) {
        (Some(s), Some(w)) => {
            // Resolve --window which may be a UUID, a name, or a numeric index.
            let sid = resolve_session_id(stream, s).await?;
            let (wid, _title) = resolve_window_id(stream, &sid, w).await?;
            params.insert("session_id".into(), serde_json::Value::String(sid));
            params.insert("window_id".into(), serde_json::Value::String(wid));
            "window.snapshot"
        }
        (None, Some(w)) => {
            // No session — `w` must be a UUID (daemon resolves directly).
            params.insert("window_id".into(), serde_json::Value::String(w.to_string()));
            "window.snapshot"
        }
        (Some(s), None) => {
            let sid = resolve_session_id(stream, s).await?;
            params.insert("session_id".into(), serde_json::Value::String(sid));
            "session.snapshot"
        }
        (None, None) => {
            anyhow::bail!("provide --session and/or --window");
        }
    };

    let result = rpc_call(stream, method, serde_json::Value::Object(params)).await?;

    let b64 = result
        .get("png_base64")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("daemon response missing png_base64"))?;
    let png = base64::engine::general_purpose::STANDARD.decode(b64)?;

    match (output, format) {
        (Some(path), _) => {
            std::fs::write(&path, &png)?;
            if !matches!(format, OutputFormat::Json) {
                let w = result.get("width").and_then(|v| v.as_u64()).unwrap_or(0);
                let h = result.get("height").and_then(|v| v.as_u64()).unwrap_or(0);
                println!(
                    "{} {} ({}×{} px, {} bytes)",
                    crate::style::success("✓ snapshot →"),
                    crate::style::bold(path.display().to_string().as_str()),
                    w,
                    h,
                    png.len(),
                );
            } else {
                println!("{}", serde_json::to_string_pretty(&result)?);
            }
        }
        (None, OutputFormat::Json) => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        (None, _) => {
            // Default no-`--output` behaviour: print base64 to stdout so the
            // command is pipe-/jq-friendly and never dumps binary control
            // bytes into a TTY. Use `--output -.png > frame.png` (or just
            // `--output frame.png`) for raw bytes.
            println!("{b64}");
        }
    }

    Ok(())
}

const STARTER_TEMPLATE: &str = r#"# `shux apply review.toml` — atomic, dry-run-able with `--dry-run`.
[session]
name = "review"

[[windows]]
title = "git"
[[windows.panes]]
command = ["lazygit"]
"#;

pub fn handle_init(root: &std::path::Path, format: OutputFormat) -> anyhow::Result<()> {
    let shux_dir = root.join(".shux");
    for sub in ["templates", "scripts", "goldens", "out"] {
        std::fs::create_dir_all(shux_dir.join(sub))?;
    }

    let gitignore_path = shux_dir.join(".gitignore");
    let mut created = Vec::new();
    if !gitignore_path.exists() {
        std::fs::write(&gitignore_path, "out/\n*.log\n")?;
        created.push(gitignore_path.clone());
    }

    let template_path = shux_dir.join("templates").join("review.toml");
    let templates_dir = shux_dir.join("templates");
    let templates_empty = std::fs::read_dir(&templates_dir)
        .map(|mut it| it.next().is_none())
        .unwrap_or(true);
    if templates_empty && !template_path.exists() {
        std::fs::write(&template_path, STARTER_TEMPLATE)?;
        created.push(template_path.clone());
    }

    match format {
        OutputFormat::Json => {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "shux_dir": shux_dir.display().to_string(),
                    "created": created.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
                }))?
            );
        }
        _ => {
            println!(
                "{} {}",
                crate::style::success("✓ scaffolded"),
                crate::style::bold(shux_dir.display().to_string().as_str()),
            );
            for path in &created {
                println!("  {} {}", crate::style::muted("+"), path.display(),);
            }
            if created.is_empty() {
                println!(
                    "  {}",
                    crate::style::muted("(already present — nothing to do)")
                );
            }
        }
    }

    Ok(())
}

/// Resolve a UUID-SHAPED session argument against the live session list:
/// id resolution FIRST (the common case — ids come from `lens.run` /
/// `session create` responses), falling back to NAME lookup when no session
/// has that id (session names may legally be UUID-shaped strings; codex P6
/// round-1 major 1 — a pure id short-circuit made such names unaddressable
/// and could mistarget). Precedence when the arg matches BOTH a real id and
/// a different session's name: the id wins, with a warning on stderr (the
/// ambiguity is cheaply detectable here since the list is already in hand).
/// When the arg matches NOTHING, its NORMALIZED form is passed through as
/// an id so the server produces its canonical not-found error.
///
/// `parsed` is the arg's parse (claude P6 round-1 extra: `Uuid::parse_str`
/// also accepts the 32-hex SIMPLE form and uppercase — session ids
/// serialize hyphenated lowercase, so the id comparison MUST go through the
/// normalized `to_string()` form, never raw string equality; the NAME
/// comparison stays raw/exact because names are arbitrary strings).
///
/// Queries with `include_scratch: true` because `lens.run` ids target
/// hidden scratch sessions (visibility for listing is not authorization to
/// act on a known id — LENS-R-041 principle).
async fn resolve_uuid_shaped_session(
    stream: &mut tokio::net::UnixStream,
    arg: &str,
    parsed: uuid::Uuid,
) -> Result<String, RpcClientError> {
    // Canonical hyphenated-lowercase form — what session ids serialize as.
    let normalized = parsed.to_string();
    let result = rpc_call(
        stream,
        "session.list",
        serde_json::json!({ "include_scratch": true }),
    )
    .await?;
    let sessions = result
        .get("sessions")
        .and_then(|v| v.as_array())
        .or_else(|| result.as_array());

    let mut id_match = false;
    let mut name_match_id: Option<String> = None;
    if let Some(sessions) = sessions {
        for s in sessions {
            let sid = s.get("id").and_then(|v| v.as_str());
            if sid == Some(normalized.as_str()) {
                id_match = true;
            }
            if s.get("name").and_then(|v| v.as_str()) == Some(arg) {
                if let Some(sid) = sid {
                    name_match_id = Some(sid.to_string());
                }
            }
        }
    }

    match (id_match, name_match_id) {
        (true, Some(name_id)) if name_id != normalized => {
            eprintln!(
                "{}",
                crate::style::warning(format!(
                    "warning: '{arg}' matches both a session ID and a different \
                     session's NAME; targeting the session with that ID (id wins). \
                     To target the session named '{arg}', pass its id: {name_id}"
                ))
            );
            Ok(normalized)
        }
        (true, _) => Ok(normalized),
        (false, Some(name_id)) => Ok(name_id),
        // No match either way: pass the normalized form through as an id so
        // the server emits its canonical not-found (clean, consistent).
        (false, None) => Ok(normalized),
    }
}

/// Resolve a session name-or-UUID to its UUID.
///
/// Accepts either form (issue #88: RPC methods already take `id` OR `name` —
/// `-s/--session` only resolved by name, so a caller holding a session UUID
/// straight from an RPC/CLI result — e.g. `lens.run`'s `session_id`, which
/// targets a SCRATCH session excluded from the default `session.list` —
/// had no CLI-side way to address it). UUID-shaped input (hyphenated OR
/// 32-hex simple form, any case — everything `Uuid::parse_str` accepts)
/// resolves as an id FIRST with name fallback; id wins when both match
/// (see `resolve_uuid_shaped_session` for the precedence rules). Non-UUID
/// input resolves by name as always.
async fn resolve_session_id(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
) -> Result<String, RpcClientError> {
    if let Ok(parsed) = uuid::Uuid::parse_str(session_name) {
        return resolve_uuid_shaped_session(stream, session_name, parsed).await;
    }
    let result = rpc_call(stream, "session.list", serde_json::json!({})).await?;
    let sessions = result
        .get("sessions")
        .and_then(|v| v.as_array())
        .or_else(|| result.as_array());

    if let Some(sessions) = sessions {
        for s in sessions {
            if s.get("name").and_then(|v| v.as_str()) == Some(session_name) {
                if let Some(id) = s.get("id").and_then(|v| v.as_str()) {
                    return Ok(id.to_string());
                }
            }
        }
    }

    Err(RpcClientError::Rpc {
        code: -32004,
        message: format!("session '{session_name}' not found"),
        data: None,
    })
}

/// Resolve a window specifier (name or index) to (window_id, window_title).
async fn resolve_window_id(
    stream: &mut tokio::net::UnixStream,
    session_id: &str,
    window_spec: &str,
) -> Result<(String, String), RpcClientError> {
    let result = rpc_call(
        stream,
        "window.list",
        serde_json::json!({"session_id": session_id}),
    )
    .await?;
    let windows = result.as_array().ok_or_else(|| RpcClientError::Rpc {
        code: -32603,
        message: "unexpected response from window.list".to_string(),
        data: None,
    })?;

    // Try as numeric index first
    if let Ok(idx) = window_spec.parse::<usize>() {
        if let Some(w) = windows.get(idx) {
            let id = w.get("id").and_then(|v| v.as_str()).unwrap_or("?");
            let title = w.get("title").and_then(|v| v.as_str()).unwrap_or("?");
            return Ok((id.to_string(), title.to_string()));
        }
    }

    // Try as window name
    for w in windows {
        if w.get("title").and_then(|v| v.as_str()) == Some(window_spec) {
            let id = w.get("id").and_then(|v| v.as_str()).unwrap_or("?");
            let title = w.get("title").and_then(|v| v.as_str()).unwrap_or("?");
            return Ok((id.to_string(), title.to_string()));
        }
    }

    Err(RpcClientError::Rpc {
        code: -32004,
        message: format!("window '{window_spec}' not found in session"),
        data: None,
    })
}

/// Handle the `shux window list` command.
pub async fn handle_window_list(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let session_id = resolve_session_id(stream, session_name).await?;
    let result = rpc_call(
        stream,
        "window.list",
        serde_json::json!({"session_id": session_id}),
    )
    .await?;

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        OutputFormat::Text | OutputFormat::Plain => {
            use crate::style;

            let ctx = style::TerminalContext::detect(to_style_format(format));

            let window_infos: Vec<style::WindowInfo> = result
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .map(|w| {
                            let index =
                                w.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                            let title = w
                                .get("title")
                                .and_then(|v| v.as_str())
                                .unwrap_or("(untitled)")
                                .to_string();
                            let pane_count =
                                w.get("pane_count").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                            let is_active = w
                                .get("is_active")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false);
                            style::WindowInfo {
                                title,
                                index,
                                pane_count,
                                is_active,
                            }
                        })
                        .collect()
                })
                .unwrap_or_default();

            style::render_window_list(&ctx, session_name, &window_infos);
        }
    }

    Ok(())
}

/// Handle the `shux window new` command.
#[allow(clippy::too_many_arguments)]
pub async fn handle_window_new(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    window_name: Option<String>,
    cwd: Option<std::path::PathBuf>,
    cmd: Option<String>,
    argv: Vec<String>,
    ensure: bool,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let session_id = resolve_session_id(stream, session_name).await?;

    let method = if ensure {
        "window.ensure"
    } else {
        "window.create"
    };
    let mut params = serde_json::Map::new();
    params.insert(
        "session_id".to_string(),
        serde_json::Value::String(session_id),
    );
    if let Some(name) = &window_name {
        params.insert("name".to_string(), serde_json::Value::String(name.clone()));
    }
    if let Some(c) = &cwd {
        params.insert(
            "cwd".to_string(),
            serde_json::Value::String(c.display().to_string()),
        );
    }
    // Trailing argv (after `--`) wins over --cmd, matching the
    // `shux session create` behavior so muscle memory carries over.
    let command_vec: Vec<String> = if !argv.is_empty() {
        argv
    } else if let Some(c) = cmd {
        vec!["sh".into(), "-c".into(), c]
    } else {
        Vec::new()
    };
    if !command_vec.is_empty() {
        params.insert(
            "command".to_string(),
            serde_json::Value::Array(
                command_vec
                    .into_iter()
                    .map(serde_json::Value::String)
                    .collect(),
            ),
        );
    }

    let result = rpc_call(stream, method, serde_json::Value::Object(params)).await?;

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        OutputFormat::Text | OutputFormat::Plain => {
            let title = result
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("(untitled)");
            let index = result.get("index").and_then(|v| v.as_u64()).unwrap_or(0);
            crate::style::print_window_created(title, index);
        }
    }

    Ok(())
}

/// Handle the `shux window kill` command.
pub async fn handle_window_kill(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    window_spec: &str,
    expected_version: Option<u64>,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let session_id = resolve_session_id(stream, session_name).await?;
    let (window_id, window_title) = resolve_window_id(stream, &session_id, window_spec).await?;

    let mut params = serde_json::Map::new();
    params.insert("id".to_string(), serde_json::Value::String(window_id));
    if let Some(ev) = expected_version {
        params.insert("expected_version".to_string(), serde_json::Value::from(ev));
    }
    let result = rpc_call(stream, "window.kill", serde_json::Value::Object(params)).await?;

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        OutputFormat::Text | OutputFormat::Plain => {
            crate::style::print_window_killed(&window_title);
        }
    }

    Ok(())
}

/// Handle the `shux window rename` command.
pub async fn handle_window_rename(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    window_spec: &str,
    new_name: &str,
    expected_version: Option<u64>,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let session_id = resolve_session_id(stream, session_name).await?;
    let (window_id, old_title) = resolve_window_id(stream, &session_id, window_spec).await?;

    let mut params = serde_json::Map::new();
    params.insert("id".to_string(), serde_json::Value::String(window_id));
    params.insert(
        "name".to_string(),
        serde_json::Value::String(new_name.to_string()),
    );
    if let Some(ev) = expected_version {
        params.insert("expected_version".to_string(), serde_json::Value::from(ev));
    }
    let result = rpc_call(stream, "window.rename", serde_json::Value::Object(params)).await?;

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        OutputFormat::Text | OutputFormat::Plain => {
            crate::style::print_window_renamed(&old_title, new_name);
        }
    }

    Ok(())
}

/// Handle the `shux window focus` command.
pub async fn handle_window_focus(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    window_spec: &str,
    expected_version: Option<u64>,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let session_id = resolve_session_id(stream, session_name).await?;
    let (window_id, window_title) = resolve_window_id(stream, &session_id, window_spec).await?;

    let mut params = serde_json::Map::new();
    params.insert("id".to_string(), serde_json::Value::String(window_id));
    if let Some(ev) = expected_version {
        params.insert("expected_version".to_string(), serde_json::Value::from(ev));
    }
    let result = rpc_call(stream, "window.focus", serde_json::Value::Object(params)).await?;

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        OutputFormat::Text | OutputFormat::Plain => {
            crate::style::print_window_focused(&window_title);
        }
    }

    Ok(())
}

/// Handle the `shux window reorder` command.
pub async fn handle_window_reorder(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    window_spec: &str,
    new_index: usize,
    expected_version: Option<u64>,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let session_id = resolve_session_id(stream, session_name).await?;
    let (window_id, window_title) = resolve_window_id(stream, &session_id, window_spec).await?;

    let mut params = serde_json::Map::new();
    params.insert("id".to_string(), serde_json::Value::String(window_id));
    params.insert(
        "new_index".to_string(),
        serde_json::Value::from(new_index as u64),
    );
    if let Some(ev) = expected_version {
        params.insert("expected_version".to_string(), serde_json::Value::from(ev));
    }
    let result = rpc_call(stream, "window.reorder", serde_json::Value::Object(params)).await?;

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        OutputFormat::Text | OutputFormat::Plain => {
            crate::style::print_window_reordered(&window_title, new_index);
        }
    }

    Ok(())
}

/// Resolve a pane-related window_id: either explicit window spec or session's active window.
async fn resolve_pane_window_id(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    window_spec: Option<&str>,
) -> Result<(String, String), RpcClientError> {
    let session_id = resolve_session_id(stream, session_name).await?;
    match window_spec {
        Some(spec) => {
            let (wid, _title) = resolve_window_id(stream, &session_id, spec).await?;
            Ok((session_id, wid))
        }
        None => {
            // Get active window from session. `include_scratch: true` so a
            // scratch session's pane (e.g. from `lens.run`'s `session_id`) is
            // still driveable without an explicit `--window` — the default
            // `session.list` visibility rule (LENS-R-041) is about listing,
            // not about whether a caller who already holds the id can act on
            // it (same "visibility != authorization" principle as the RPC).
            let result = rpc_call(
                stream,
                "session.list",
                serde_json::json!({ "include_scratch": true }),
            )
            .await?;
            let sessions = result
                .get("sessions")
                .and_then(|v| v.as_array())
                .or_else(|| result.as_array());
            if let Some(sessions) = sessions {
                for s in sessions {
                    if s.get("id").and_then(|v| v.as_str()) == Some(&session_id) {
                        if let Some(aw) = s.get("active_window_id").and_then(|v| v.as_str()) {
                            return Ok((session_id, aw.to_string()));
                        }
                    }
                }
            }
            Err(RpcClientError::Rpc {
                code: -32004,
                message: "could not determine active window".to_string(),
                data: None,
            })
        }
    }
}

async fn validate_pane_belongs_to_session(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    pane_id: &str,
) -> Result<(), RpcClientError> {
    let session_id = resolve_session_id(stream, session_name).await?;
    let windows = rpc_call(
        stream,
        "window.list",
        serde_json::json!({"session_id": session_id}),
    )
    .await?;
    let Some(windows) = windows.as_array() else {
        return Err(RpcClientError::Rpc {
            code: -32004,
            message: "could not list session windows".to_string(),
            data: None,
        });
    };

    for window in windows {
        let Some(window_id) = window.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        let panes = rpc_call(
            stream,
            "pane.list",
            serde_json::json!({"session_id": session_id, "window_id": window_id}),
        )
        .await?;
        if panes.as_array().is_some_and(|panes| {
            panes
                .iter()
                .any(|p| p.get("id").and_then(|v| v.as_str()) == Some(pane_id))
        }) {
            return Ok(());
        }
    }

    Err(RpcClientError::Rpc {
        code: -32004,
        message: format!("pane {pane_id} does not belong to session {session_name}"),
        data: None,
    })
}

/// Handle the `shux pane list` command.
pub async fn handle_pane_list(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    window_spec: Option<&str>,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let (session_id, window_id) = resolve_pane_window_id(stream, session_name, window_spec).await?;

    let result = rpc_call(
        stream,
        "pane.list",
        serde_json::json!({"session_id": session_id, "window_id": window_id}),
    )
    .await?;

    // Resolve the window title for the header
    let window_title = {
        let win_result = rpc_call(
            stream,
            "window.list",
            serde_json::json!({"session_id": session_id}),
        )
        .await
        .ok();
        win_result
            .and_then(|r| {
                r.as_array().and_then(|windows| {
                    windows.iter().find_map(|w| {
                        let wid = w.get("id").and_then(|v| v.as_str()).unwrap_or("");
                        if wid == window_id {
                            w.get("title").and_then(|v| v.as_str()).map(String::from)
                        } else {
                            None
                        }
                    })
                })
            })
            .unwrap_or_else(|| window_id.chars().take(8).collect())
    };

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        OutputFormat::Text | OutputFormat::Plain => {
            use crate::style;

            let ctx = style::TerminalContext::detect(to_style_format(format));

            let pane_infos: Vec<style::PaneInfo> = result
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .map(|p| {
                            let id = p
                                .get("id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("?")
                                .to_string();
                            let cwd = p
                                .get("cwd")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let command = p
                                .get("command")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let is_focused = p
                                .get("is_focused")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false);
                            let is_zoomed = p
                                .get("is_zoomed")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false);
                            style::PaneInfo {
                                id,
                                cwd,
                                command,
                                is_focused,
                                is_zoomed,
                            }
                        })
                        .collect()
                })
                .unwrap_or_default();

            style::render_pane_list(&ctx, session_name, &window_title, &pane_infos);
        }
    }

    Ok(())
}

/// Handle the `shux pane split` command.
pub async fn handle_pane_split(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    window_spec: Option<&str>,
    pane_spec: Option<&str>,
    direction: Option<&str>,
    ratio: Option<f64>,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let (session_id, window_id) = resolve_pane_window_id(stream, session_name, window_spec).await?;

    let mut params = serde_json::json!({
        "session_id": session_id,
        "window_id": window_id,
    });

    if let Some(pid) = pane_spec {
        params["pane_id"] = serde_json::Value::String(pid.to_string());
    }
    if let Some(dir) = direction {
        params["direction"] = serde_json::Value::String(dir.to_string());
    }
    if let Some(r) = ratio {
        params["ratio"] = serde_json::json!(r);
    }

    let result = rpc_call(stream, "pane.split", params).await?;

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        OutputFormat::Text | OutputFormat::Plain => {
            let pane_id = result
                .get("pane")
                .and_then(|v| v.get("id"))
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let dir_label = direction.unwrap_or("vertical");
            crate::style::print_pane_split(pane_id, dir_label);
        }
    }

    Ok(())
}

/// Handle the `shux pane focus` command.
pub async fn handle_pane_focus(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    window_spec: Option<&str>,
    pane_id: &str,
    format: OutputFormat,
) -> anyhow::Result<()> {
    // Resolve window for validation, but pane.focus only needs pane_id
    let _ = resolve_pane_window_id(stream, session_name, window_spec).await?;

    let result = rpc_call(
        stream,
        "pane.focus",
        serde_json::json!({"pane_id": pane_id}),
    )
    .await?;

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        OutputFormat::Text | OutputFormat::Plain => {
            crate::style::print_pane_focused(pane_id);
        }
    }

    Ok(())
}

/// Handle the `shux pane focus-dir` command.
pub async fn handle_pane_focus_dir(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    window_spec: Option<&str>,
    direction: &str,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let (session_id, window_id) = resolve_pane_window_id(stream, session_name, window_spec).await?;

    let result = rpc_call(
        stream,
        "pane.focus_direction",
        serde_json::json!({
            "session_id": session_id,
            "window_id": window_id,
            "direction": direction,
        }),
    )
    .await?;

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        OutputFormat::Text | OutputFormat::Plain => {
            let pane_id = result
                .get("pane_id")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            crate::style::print_pane_focused(pane_id);
        }
    }

    Ok(())
}

/// Handle the `shux pane resize` command.
#[allow(clippy::too_many_arguments)]
pub async fn handle_pane_resize(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    window_spec: Option<&str>,
    pane_spec: Option<&str>,
    direction: &str,
    delta: Option<f64>,
    expected_version: Option<u64>,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let (session_id, window_id) = resolve_pane_window_id(stream, session_name, window_spec).await?;

    let mut params = serde_json::json!({
        "session_id": session_id,
        "window_id": window_id,
        "direction": direction,
    });

    if let Some(pid) = pane_spec {
        params["pane_id"] = serde_json::Value::String(pid.to_string());
    }
    if let Some(d) = delta {
        params["delta"] = serde_json::json!(d);
    }
    if let Some(ev) = expected_version {
        params["expected_version"] = serde_json::Value::from(ev);
    }

    let result = rpc_call(stream, "pane.resize", params).await?;

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        OutputFormat::Text | OutputFormat::Plain => {
            let pane_id = result
                .get("pane_id")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            crate::style::print_pane_resized(pane_id);
        }
    }

    Ok(())
}

/// Handle the `shux pane zoom` command.
pub async fn handle_pane_zoom(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    window_spec: Option<&str>,
    pane_spec: Option<&str>,
    expected_version: Option<u64>,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let (session_id, window_id) = resolve_pane_window_id(stream, session_name, window_spec).await?;

    let mut params = serde_json::json!({
        "session_id": session_id,
        "window_id": window_id,
    });

    if let Some(pid) = pane_spec {
        params["pane_id"] = serde_json::Value::String(pid.to_string());
    }
    if let Some(ev) = expected_version {
        params["expected_version"] = serde_json::Value::from(ev);
    }

    let result = rpc_call(stream, "pane.zoom", params).await?;

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        OutputFormat::Text | OutputFormat::Plain => {
            let pane_id = result
                .get("pane_id")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let is_zoomed = result
                .get("is_zoomed")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            crate::style::print_pane_zoomed(pane_id, is_zoomed);
        }
    }

    Ok(())
}

/// Handle the `shux pane swap` command.
pub async fn handle_pane_swap(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    window_spec: Option<&str>,
    pane_id: &str,
    target_id: &str,
    expected_version: Option<u64>,
    format: OutputFormat,
) -> anyhow::Result<()> {
    // Resolve window for validation
    let _ = resolve_pane_window_id(stream, session_name, window_spec).await?;

    let mut params = serde_json::Map::new();
    params.insert(
        "pane_id".to_string(),
        serde_json::Value::String(pane_id.to_string()),
    );
    params.insert(
        "target_pane_id".to_string(),
        serde_json::Value::String(target_id.to_string()),
    );
    if let Some(ev) = expected_version {
        params.insert("expected_version".to_string(), serde_json::Value::from(ev));
    }
    let result = rpc_call(stream, "pane.swap", serde_json::Value::Object(params)).await?;

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        OutputFormat::Text | OutputFormat::Plain => {
            crate::style::print_pane_swapped(pane_id, target_id);
        }
    }

    Ok(())
}

/// Handle `shux pane title` — set or clear a pane title.
///
/// `--title "..."` sets a manual override; `--clear` removes it.
/// `--auto` / `--no-auto` toggle whether OSC + command-derived
/// titles flow into the displayed title (orthogonal to the manual
/// override, so you can pin auto OFF without clearing your manual
/// title).
#[allow(clippy::too_many_arguments)]
pub async fn handle_pane_title(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    window_spec: Option<&str>,
    pane_spec: Option<&str>,
    title: Option<&str>,
    clear: bool,
    auto: bool,
    no_auto: bool,
    format: OutputFormat,
) -> anyhow::Result<()> {
    if title.is_some() && clear {
        anyhow::bail!("--title and --clear are mutually exclusive");
    }
    if auto && no_auto {
        anyhow::bail!("--auto and --no-auto are mutually exclusive");
    }

    let (session_id, window_id) = resolve_pane_window_id(stream, session_name, window_spec).await?;

    let mut params = serde_json::json!({
        "session_id": session_id,
        "window_id": window_id,
    });
    if let Some(pid) = pane_spec {
        params["pane_id"] = serde_json::Value::String(pid.to_string());
    }
    // Title intent: explicit `null` clears, string sets, omitted leaves
    // manual_title unchanged. clap can't directly emit that tri-state
    // for us — we synthesize it here.
    if clear {
        params["title"] = serde_json::Value::Null;
    } else if let Some(t) = title {
        params["title"] = serde_json::Value::String(t.to_string());
    }
    if auto {
        params["auto"] = serde_json::Value::Bool(true);
    } else if no_auto {
        params["auto"] = serde_json::Value::Bool(false);
    }

    let result = rpc_call(stream, "pane.set_title", params).await?;

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        OutputFormat::Text | OutputFormat::Plain => {
            let pid = result
                .get("pane_id")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let displayed = result.get("title").and_then(|v| v.as_str()).unwrap_or("");
            crate::style::print_pane_title_set(pid, displayed);
        }
    }

    Ok(())
}

/// Handle `shux pane watch` — long-poll `pane.output.watch` and write
/// each chunk's bytes to stdout. Pipes cleanly into `tee log` etc.
/// PR 2c / data-plane consumer.
pub async fn handle_pane_watch(
    stream: &mut tokio::net::UnixStream,
    _session_name: &str,
    pane_id: &str,
    timeout_ms: u64,
    limit: Option<u64>,
    format: OutputFormat,
) -> anyhow::Result<()> {
    use base64::Engine;
    use std::io::Write;

    // Validate the UUID early so we fail fast on typos instead of
    // round-tripping to the daemon to discover an invalid_params.
    let _: uuid::Uuid = pane_id
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid pane uuid: {e}"))?;

    let mut next_seq: Option<u64> = None;
    let mut delivered: u64 = 0;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut warned_sampled = false;

    loop {
        let mut params = serde_json::json!({
            "pane_id": pane_id,
            "timeout_ms": timeout_ms,
            // 50 chunks per poll is plenty given the 10/s/pane source
            // rate; smaller bounds just mean more RPC round-trips.
            "limit": 50,
        });
        if let Some(s) = next_seq {
            params["from_seq"] = serde_json::json!(s);
        }
        let resp = rpc_call(stream, "pane.output.watch", params).await?;

        if let Some(arr) = resp.get("chunks").and_then(|v| v.as_array()) {
            for chunk in arr {
                let bytes_b64 = chunk.get("bytes").and_then(|v| v.as_str()).unwrap_or("");
                let sampled = chunk
                    .get("sampled")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                match format {
                    OutputFormat::Json => {
                        let _ = writeln!(out, "{}", serde_json::to_string(chunk)?);
                    }
                    OutputFormat::Text | OutputFormat::Plain => {
                        if sampled && !warned_sampled {
                            eprintln!(
                                "{} sampled pane.output chunk — bytes were dropped before this chunk; use `shux pane record --to FILE` for lossless audits",
                                crate::style::warning("!"),
                            );
                            warned_sampled = true;
                        } else if !sampled {
                            warned_sampled = false;
                        }
                        if let Ok(raw) =
                            base64::engine::general_purpose::STANDARD.decode(bytes_b64.as_bytes())
                        {
                            let _ = out.write_all(&raw);
                        }
                    }
                }
                delivered += 1;
                if let Some(lim) = limit {
                    if delivered >= lim {
                        let _ = out.flush();
                        return Ok(());
                    }
                }
            }
            let _ = out.flush();
        }
        if let Some(s) = resp.get("next_seq").and_then(|v| v.as_u64()) {
            next_seq = Some(s);
        }
        // `lagged`: surface to stderr so pipes stay clean.
        if resp
            .get("lagged")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            eprintln!(
                "{} subscriber lagged behind data plane — some chunks dropped",
                crate::style::warning("!"),
            );
        }
    }
}

/// Handle `shux pane record` — start a daemon-side lossless recorder, wait for
/// a bounded duration or Ctrl-C, then stop and report the byte count.
pub async fn handle_pane_record(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    pane_id: &str,
    to: &std::path::Path,
    force: bool,
    duration_ms: Option<u64>,
    format: OutputFormat,
) -> anyhow::Result<()> {
    // Validate the UUID early so typos don't create files.
    let _: uuid::Uuid = pane_id
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid pane uuid: {e}"))?;
    validate_pane_belongs_to_session(stream, session_name, pane_id).await?;

    let path = if to.is_absolute() {
        to.to_path_buf()
    } else {
        std::env::current_dir()?.join(to)
    };

    let start = rpc_call(
        stream,
        "pane.record.start",
        serde_json::json!({
            "pane_id": pane_id,
            "path": path,
            "overwrite": force,
            "duration_ms": duration_ms,
        }),
    )
    .await?;
    let recording_id = start
        .get("recording_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("daemon did not return recording_id"))?
        .to_string();

    match duration_ms {
        Some(ms) => tokio::time::sleep(std::time::Duration::from_millis(ms)).await,
        None => {
            eprintln!(
                "{} recording lossless pane output; press Ctrl-C to stop",
                crate::style::muted("..."),
            );
            tokio::signal::ctrl_c().await?;
        }
    }

    let stopped = rpc_call(
        stream,
        "pane.record.stop",
        serde_json::json!({
            "recording_id": recording_id,
        }),
    )
    .await?;

    match format {
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&stopped)?),
        OutputFormat::Plain => {
            let path = stopped.get("path").and_then(|v| v.as_str()).unwrap_or("?");
            let bytes = stopped
                .get("bytes_written")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let status = stopped
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            println!("recording\t{status}\t{path}\t{bytes}");
        }
        OutputFormat::Text => {
            let path = stopped.get("path").and_then(|v| v.as_str()).unwrap_or("?");
            let bytes = stopped
                .get("bytes_written")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let status = stopped
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            println!(
                "{} {} bytes to {} ({})",
                crate::style::success("✓ recorded"),
                crate::style::bold(&bytes.to_string()),
                crate::style::muted(path),
                status,
            );
        }
    }

    Ok(())
}

/// Handle the `shux pane kill` command.
pub async fn handle_pane_kill(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    window_spec: Option<&str>,
    pane_id: &str,
    expected_version: Option<u64>,
    format: OutputFormat,
) -> anyhow::Result<()> {
    // Resolve window for validation
    let _ = resolve_pane_window_id(stream, session_name, window_spec).await?;

    let mut params = serde_json::Map::new();
    params.insert(
        "pane_id".to_string(),
        serde_json::Value::String(pane_id.to_string()),
    );
    if let Some(ev) = expected_version {
        params.insert("expected_version".to_string(), serde_json::Value::from(ev));
    }
    let result = rpc_call(stream, "pane.kill", serde_json::Value::Object(params)).await?;

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        OutputFormat::Text | OutputFormat::Plain => {
            crate::style::print_pane_killed(pane_id);
        }
    }

    Ok(())
}

/// Handle the `shux rpc call <method> --params ...` command.
pub async fn handle_api(
    stream: &mut tokio::net::UnixStream,
    method: &str,
    params_str: &str,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let params: serde_json::Value = serde_json::from_str(params_str)
        .map_err(|e| anyhow::anyhow!("Invalid JSON params: {e}"))?;

    // PR 3b: surface RPC errors as part of the JSON-RPC envelope on
    // stdout, not as a human-readable anyhow error on stderr. Callers
    // of `shux rpc call` are debug tools / agents that expect to parse the
    // raw `{result | error}` shape — including bounded `data` fields
    // like `expected_version` / `actual_version` for retry loops.
    match rpc_call(stream, method, params).await {
        Ok(result) => {
            let envelope = serde_json::json!({"result": result});
            match format {
                OutputFormat::Json | OutputFormat::Text | OutputFormat::Plain => {
                    println!("{}", serde_json::to_string_pretty(&envelope)?);
                }
            }
            Ok(())
        }
        Err(RpcClientError::Rpc {
            code,
            message,
            data,
        }) => {
            let mut err_obj = serde_json::json!({
                "code": code,
                "message": message,
            });
            if let Some(d) = data {
                err_obj["data"] = d;
            }
            let envelope = serde_json::json!({"error": err_obj});
            println!("{}", serde_json::to_string_pretty(&envelope)?);
            // Non-zero exit so shell pipelines can branch, but the
            // structured error is still on stdout for parsers.
            std::process::exit(2);
        }
        Err(other) => Err(other.into()),
    }
}

/// Handle the `shux pane send-keys` command.
pub async fn handle_pane_send_keys(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    window_spec: Option<&str>,
    pane_spec: Option<&str>,
    text: Option<&str>,
    data: Option<&str>,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let (session_id, window_id) = resolve_pane_window_id(stream, session_name, window_spec).await?;

    let mut params = serde_json::json!({
        "session_id": session_id,
        "window_id": window_id,
    });

    if let Some(pid) = pane_spec {
        params["pane_id"] = serde_json::Value::String(pid.to_string());
    }

    if let Some(t) = text {
        params["text"] = serde_json::Value::String(t.to_string());
    } else if let Some(d) = data {
        params["data"] = serde_json::Value::String(d.to_string());
    } else {
        anyhow::bail!("either --text or --data must be provided");
    }

    let result = rpc_call(stream, "pane.send_keys", params).await?;

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        OutputFormat::Text | OutputFormat::Plain => {
            let bytes = result
                .get("bytes_written")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let pane_id = result
                .get("pane_id")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            crate::style::print_send_keys(pane_id, bytes);
        }
    }

    Ok(())
}

/// Handle the `shux pane run` command.
#[allow(clippy::too_many_arguments)]
pub async fn handle_pane_run(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    window_spec: Option<&str>,
    pane_spec: Option<&str>,
    command: &str,
    timeout: u64,
    is_async: bool,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let (session_id, window_id) = resolve_pane_window_id(stream, session_name, window_spec).await?;

    let mut params = serde_json::json!({
        "session_id": session_id,
        "window_id": window_id,
        "command": command,
        "timeout": timeout,
        "async": is_async,
    });

    if let Some(pid) = pane_spec {
        params["pane_id"] = serde_json::Value::String(pid.to_string());
    }

    let result = rpc_call(stream, "pane.run_command", params).await?;

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        OutputFormat::Text | OutputFormat::Plain => {
            crate::style::print_run_command(&result, is_async);
        }
    }

    Ok(())
}

/// Handle the `shux pane capture` command.
/// `shux pane snapshot` — rasterize a single pane (no chrome) via
/// `pane.snapshot` RPC. Snapshot dimensions come from the pane's
/// current VT grid size; use `pane.set_size` first to change them.
pub async fn handle_pane_snapshot(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    window_spec: Option<&str>,
    pane_spec: Option<&str>,
    output: Option<std::path::PathBuf>,
    format: OutputFormat,
) -> anyhow::Result<()> {
    use base64::Engine;

    let (session_id, window_id) = resolve_pane_window_id(stream, session_name, window_spec).await?;
    let mut params = serde_json::json!({
        "session_id": session_id,
        "window_id": window_id,
    });
    if let Some(pid) = pane_spec {
        params["pane_id"] = serde_json::Value::String(pid.to_string());
    }

    let result = rpc_call(stream, "pane.snapshot", params).await?;
    let b64 = result
        .get("png_base64")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("daemon response missing png_base64"))?;
    let png = base64::engine::general_purpose::STANDARD.decode(b64)?;

    match (output, format) {
        (Some(path), _) => {
            std::fs::write(&path, &png)?;
            if !matches!(format, OutputFormat::Json) {
                let w = result.get("width").and_then(|v| v.as_u64()).unwrap_or(0);
                let h = result.get("height").and_then(|v| v.as_u64()).unwrap_or(0);
                println!(
                    "{} {} ({}×{} px, {} bytes)",
                    crate::style::success("✓ snapshot →"),
                    crate::style::bold(path.display().to_string().as_str()),
                    w,
                    h,
                    png.len(),
                );
            } else {
                println!("{}", serde_json::to_string_pretty(&result)?);
            }
        }
        (None, OutputFormat::Json) => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        (None, _) => {
            println!("{b64}");
        }
    }
    Ok(())
}

/// Map a `pane.glance` RPC error code to its CLI exit code (lens PRD §10
/// exit-code table). `pane.glance`'s error surface is INVALID_PARAMS,
/// PANE_NOT_FOUND, PERMISSION_DENIED, PAYLOAD_TOO_LARGE — everything else
/// falls into the table's generic "any other RPC error" bucket.
fn lens_glance_exit_code(rpc_error_code: i64) -> i32 {
    match rpc_error_code {
        -32602 => 2, // INVALID_PARAMS
        -32005 => 4, // PERMISSION_DENIED
        -32013 => 5, // PAYLOAD_TOO_LARGE
        _ => 3,      // any other RPC error, incl. PANE_NOT_FOUND (-32004)
    }
}

/// `shux pane glance` — atomic {png, text, revision} of one pane via
/// `pane.glance` RPC (lens PRD §5, §10). No session/window resolution:
/// `pane` is always a raw pane UUID, mirroring the RPC's `pane_id` param.
#[allow(clippy::too_many_arguments)]
pub async fn handle_pane_glance(
    stream: &mut tokio::net::UnixStream,
    pane: &str,
    png_path: Option<std::path::PathBuf>,
    text_only: bool,
    no_cursor: bool,
    checkpoint: bool,
    include_cells: bool,
    cells_out: Option<std::path::PathBuf>,
    masks: Vec<(u16, u16, u16)>,
    format: OutputFormat,
) -> anyhow::Result<()> {
    // clap's `conflicts_with` already rejects this combination at parse time
    // (exit 2, no RPC); this guard keeps the invariant for any programmatic
    // caller of the handler (greptile PR #89 P2).
    if text_only && png_path.is_some() {
        anyhow::bail!("--text-only and --png are mutually exclusive");
    }
    let mask_params: Vec<serde_json::Value> = masks
        .iter()
        .map(|(row, col, width)| serde_json::json!({"row": row, "col": col, "width": width}))
        .collect();
    let params = serde_json::json!({
        "pane_id": pane,
        "include_cursor": !no_cursor,
        "include_png": !text_only,
        "checkpoint": checkpoint,
        "include_cells": include_cells,
        "masks": mask_params,
    });

    match rpc_call(stream, "pane.glance", params).await {
        Ok(result) => {
            // Write the canonical `cells` envelope to disk when requested (task 080).
            if let Some(path) = &cells_out {
                let Some(cells) = result.get("cells") else {
                    anyhow::bail!("--cells-out given but the glance result has no cells field");
                };
                std::fs::write(path, format!("{}\n", serde_json::to_string_pretty(cells)?))?;
            }
            if let Some(path) = &png_path {
                use base64::Engine;
                let b64 = result.get("png_base64").and_then(|v| v.as_str());
                let Some(b64) = b64 else {
                    anyhow::bail!(
                        "--png given but the glance result has no png_base64 \
                         (was --text-only also passed?)"
                    );
                };
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(b64)
                    .map_err(|e| anyhow::anyhow!("decode glance png: {e}"))?;
                std::fs::write(path, &bytes)?;
            }

            match format {
                OutputFormat::Json => {
                    // Deliberately the `{result|error}` envelope, NOT the bare
                    // result the sibling snapshot/capture handlers emit: the
                    // FROZEN lens harness (lens_common::cli_envelope, its doc
                    // comment reads §10 as "the raw RPC result envelope")
                    // parses `.get("result")/.get("error")` from every lens
                    // CLI verb's --format json output, giving byte-parity
                    // with `shux rpc call` (M9). Emitting the bare result
                    // breaks G1/G2/G2w CLI twins (verified empirically —
                    // codex P2 review minor 4 is DISPUTED with that
                    // evidence; changing shape requires a LENS-TEST-CHANGE
                    // to the frozen harness first).
                    let envelope = serde_json::json!({"result": result});
                    println!("{}", serde_json::to_string_pretty(&envelope)?);
                }
                OutputFormat::Text | OutputFormat::Plain => {
                    let revision = result.get("revision").and_then(|v| v.as_u64()).unwrap_or(0);
                    let cols = result.get("cols").and_then(|v| v.as_u64()).unwrap_or(0);
                    let rows = result.get("rows").and_then(|v| v.as_u64()).unwrap_or(0);
                    let cursor = result.get("cursor").cloned().unwrap_or_default();
                    let cursor_row = cursor.get("row").and_then(|v| v.as_u64()).unwrap_or(0);
                    let cursor_col = cursor.get("col").and_then(|v| v.as_u64()).unwrap_or(0);
                    let cursor_visible = cursor
                        .get("visible")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let alt_screen = result
                        .get("alt_screen")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let checkpointed = result
                        .get("checkpointed")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let evicted_revision = result.get("evicted_revision").and_then(|v| v.as_u64());
                    let text = result.get("text").and_then(|v| v.as_str()).unwrap_or("");
                    let png_written = png_path.as_deref().map(|p| {
                        let len = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
                        (p, len)
                    });
                    crate::style::print_pane_glance(
                        pane,
                        revision,
                        cols,
                        rows,
                        cursor_row,
                        cursor_col,
                        cursor_visible,
                        alt_screen,
                        checkpointed,
                        evicted_revision,
                        text,
                        png_written,
                    );
                }
            }
            Ok(())
        }
        Err(RpcClientError::Rpc {
            code,
            message,
            data,
        }) => {
            match format {
                OutputFormat::Json => {
                    let mut err_obj = serde_json::json!({
                        "code": code,
                        "message": message,
                    });
                    if let Some(d) = data {
                        err_obj["data"] = d;
                    }
                    let envelope = serde_json::json!({"error": err_obj});
                    println!("{}", serde_json::to_string_pretty(&envelope)?);
                }
                OutputFormat::Text | OutputFormat::Plain => {
                    crate::style::print_error(&format!("glance failed: {message} (code {code})"));
                }
            }
            std::process::exit(lens_glance_exit_code(code));
        }
        Err(other) => Err(other.into()),
    }
}

/// Map a `pane.wait_settled` RPC error code to its CLI exit code (lens PRD
/// §10). A settle TIMEOUT is NOT an error — it is a `settled=false` RESULT
/// handled in the success arm below and mapped to exit 1 there. This maps only
/// genuine RPC errors: INVALID_PARAMS → 2, PERMISSION_DENIED → 4, everything
/// else (incl. PANE_NOT_FOUND) → 3.
fn lens_wait_settled_exit_code(rpc_error_code: i64) -> i32 {
    match rpc_error_code {
        -32602 => 2, // INVALID_PARAMS
        -32005 => 4, // PERMISSION_DENIED
        _ => 3,      // any other RPC error, incl. PANE_NOT_FOUND (-32004)
    }
}

/// `shux pane wait-settled` — block until a pane is quiet via
/// `pane.wait_settled` RPC (lens PRD §6, §10). `quiet`/`timeout` arrive here
/// already normalized to milliseconds by `parse_duration_ms`. Exit 0 when
/// settled, exit 1 on timeout (`settled=false`, a RESULT not an error).
pub async fn handle_pane_wait_settled(
    stream: &mut tokio::net::UnixStream,
    pane: &str,
    quiet_ms: u64,
    timeout_ms: u64,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let params = serde_json::json!({
        "pane_id": pane,
        "quiet_ms": quiet_ms,
        "timeout_ms": timeout_ms,
    });

    match rpc_call(stream, "pane.wait_settled", params).await {
        Ok(result) => {
            let settled = result
                .get("settled")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            match format {
                OutputFormat::Json => {
                    // §10: byte-identical to `shux rpc call` — the `{result}`
                    // envelope (the frozen lens harness parses this shape).
                    let envelope = serde_json::json!({ "result": result });
                    println!("{}", serde_json::to_string_pretty(&envelope)?);
                }
                OutputFormat::Text | OutputFormat::Plain => {
                    let revision = result.get("revision").and_then(|v| v.as_u64()).unwrap_or(0);
                    let waited_ms = result
                        .get("waited_ms")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    crate::style::print_pane_wait_settled(pane, settled, revision, waited_ms);
                }
            }
            // §10 CLI-only mapping: settled → exit 0, timeout → exit 1.
            if settled {
                Ok(())
            } else {
                std::process::exit(1);
            }
        }
        Err(RpcClientError::Rpc {
            code,
            message,
            data,
        }) => {
            match format {
                OutputFormat::Json => {
                    let mut err_obj = serde_json::json!({
                        "code": code,
                        "message": message,
                    });
                    if let Some(d) = data {
                        err_obj["data"] = d;
                    }
                    let envelope = serde_json::json!({ "error": err_obj });
                    println!("{}", serde_json::to_string_pretty(&envelope)?);
                }
                OutputFormat::Text | OutputFormat::Plain => {
                    crate::style::print_error(&format!(
                        "wait-settled failed: {message} (code {code})"
                    ));
                }
            }
            std::process::exit(lens_wait_settled_exit_code(code));
        }
        Err(other) => Err(other.into()),
    }
}

/// Map a `pane.checkpoint` RPC error code to its CLI exit code (lens PRD §10).
/// `pane.checkpoint` error surface: INVALID_PARAMS (bad/missing pane_id →
/// exit 2), PERMISSION_DENIED (exit 4), PANE_NOT_FOUND + anything else → exit 3.
fn lens_checkpoint_exit_code(rpc_error_code: i64) -> i32 {
    match rpc_error_code {
        -32602 => 2, // INVALID_PARAMS
        -32005 => 4, // PERMISSION_DENIED
        _ => 3,      // any other RPC error, incl. PANE_NOT_FOUND (-32004)
    }
}

/// Map a `pane.diff_since` RPC error code to its CLI exit code (lens PRD §10
/// exit-code table). STALE_REVISION / RESIZE_INVALIDATED / PAYLOAD_TOO_LARGE
/// map to exit 5 (diff-specific data errors); INVALID_PARAMS → 2,
/// PERMISSION_DENIED → 4, everything else (incl. PANE_NOT_FOUND) → 3.
fn lens_diff_exit_code(rpc_error_code: i64) -> i32 {
    match rpc_error_code {
        -32602 => 2,                   // INVALID_PARAMS
        -32005 => 4,                   // PERMISSION_DENIED
        -32010 | -32011 | -32013 => 5, // STALE / INVALIDATED / PAYLOAD_TOO_LARGE
        _ => 3,                        // any other RPC error, incl. PANE_NOT_FOUND
    }
}

/// Emit the `{error}` envelope (`--format json`) or a styled error line, then
/// exit with `exit_code`. Shared by the checkpoint/diff error arms — byte-
/// parity with `shux rpc call` (M9), the shape the frozen lens harness parses.
fn lens_emit_error_and_exit(
    format: OutputFormat,
    verb: &str,
    code: i64,
    message: &str,
    data: Option<serde_json::Value>,
    exit_code: i32,
) -> ! {
    match format {
        OutputFormat::Json => {
            let mut err_obj = serde_json::json!({ "code": code, "message": message });
            if let Some(d) = data {
                err_obj["data"] = d;
            }
            let envelope = serde_json::json!({ "error": err_obj });
            match serde_json::to_string_pretty(&envelope) {
                Ok(s) => println!("{s}"),
                Err(e) => eprintln!("failed to serialize error envelope: {e}"),
            }
        }
        OutputFormat::Text | OutputFormat::Plain => {
            crate::style::print_error(&format!("{verb} failed: {message} (code {code})"));
        }
    }
    std::process::exit(exit_code);
}

/// `shux pane checkpoint` — capture a checkpoint via `pane.checkpoint` RPC
/// (lens PRD §7, §10). `pane` is a raw pane UUID, mirroring the RPC `pane_id`.
pub async fn handle_pane_checkpoint(
    stream: &mut tokio::net::UnixStream,
    pane: &str,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let params = serde_json::json!({ "pane_id": pane });

    match rpc_call(stream, "pane.checkpoint", params).await {
        Ok(result) => {
            match format {
                OutputFormat::Json => {
                    // §10: the `{result}` envelope, byte-identical to
                    // `shux rpc call` (the frozen lens harness parses this).
                    let envelope = serde_json::json!({ "result": result });
                    println!("{}", serde_json::to_string_pretty(&envelope)?);
                }
                OutputFormat::Text | OutputFormat::Plain => {
                    let revision = result.get("revision").and_then(|v| v.as_u64()).unwrap_or(0);
                    let evicted = result.get("evicted_revision").and_then(|v| v.as_u64());
                    crate::style::print_pane_checkpoint(pane, revision, evicted);
                }
            }
            Ok(())
        }
        Err(RpcClientError::Rpc {
            code,
            message,
            data,
        }) => lens_emit_error_and_exit(
            format,
            "checkpoint",
            code,
            &message,
            data,
            lens_checkpoint_exit_code(code),
        ),
        Err(other) => Err(other.into()),
    }
}

/// `shux pane diff` — structured diff via `pane.diff_since` RPC (lens PRD §7,
/// §10). `--heat <path>` writes the heat PNG; `--no-row-text` drops the
/// per-row changed text. Exit 0 on any delta; exit 5 on stale/invalidated.
pub async fn handle_pane_diff(
    stream: &mut tokio::net::UnixStream,
    pane: &str,
    since: u64,
    heat_path: Option<std::path::PathBuf>,
    no_row_text: bool,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let params = serde_json::json!({
        "pane_id": pane,
        "since_revision": since,
        "changed_row_text": !no_row_text,
        // Only request the heat PNG when the caller wants a file for it.
        "heat_png": heat_path.is_some(),
    });

    match rpc_call(stream, "pane.diff_since", params).await {
        Ok(result) => {
            if let Some(path) = &heat_path {
                use base64::Engine;
                let b64 = result.get("heat_png_base64").and_then(|v| v.as_str());
                let Some(b64) = b64 else {
                    anyhow::bail!("--heat given but the diff result has no heat_png_base64");
                };
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(b64)
                    .map_err(|e| anyhow::anyhow!("decode heat png: {e}"))?;
                std::fs::write(path, &bytes)?;
            }

            match format {
                OutputFormat::Json => {
                    // §10: the `{result}` envelope, byte-identical to
                    // `shux rpc call` (the frozen lens harness parses this).
                    let envelope = serde_json::json!({ "result": result });
                    println!("{}", serde_json::to_string_pretty(&envelope)?);
                }
                OutputFormat::Text | OutputFormat::Plain => {
                    let from = result
                        .get("from_revision")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let to = result
                        .get("to_revision")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let cells = result
                        .get("cells_changed")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let cursor_moved = result
                        .get("cursor_moved")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let regions = result
                        .get("regions")
                        .and_then(|v| v.as_array())
                        .map(|a| a.len())
                        .unwrap_or(0);
                    let truncated = result
                        .get("regions_truncated")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let heat_written = heat_path.as_deref().map(|p| {
                        let len = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
                        (p, len)
                    });
                    crate::style::print_pane_diff(
                        pane,
                        from,
                        to,
                        cells,
                        regions,
                        truncated,
                        cursor_moved,
                        heat_written,
                    );
                }
            }
            // Exit 0 on ANY delta — the diff is data, not a verdict (§10).
            Ok(())
        }
        Err(RpcClientError::Rpc {
            code,
            message,
            data,
        }) => lens_emit_error_and_exit(
            format,
            "diff",
            code,
            &message,
            data,
            lens_diff_exit_code(code),
        ),
        Err(other) => Err(other.into()),
    }
}

/// Map a `lens.run` RPC error code to its CLI exit code (lens PRD §10 exit
/// table): INVALID_PARAMS → 2, PERMISSION_DENIED → 4,
/// RESOURCE_EXHAUSTED/SPAWN_FAILED → 5 (setup failures BEFORE the child
/// starts — the child-exit-code precedence rule only applies once `wait`
/// has actually observed the process start), everything else → 3.
fn lens_run_exit_code(rpc_error_code: i64) -> i32 {
    match rpc_error_code {
        -32602 => 2,          // INVALID_PARAMS
        -32005 => 4,          // PERMISSION_DENIED
        -32012 | -32014 => 5, // RESOURCE_EXHAUSTED / SPAWN_FAILED
        _ => 3,
    }
}

/// `shux lens run` — spawn `argv` in a hidden scratch session via `lens.run`
/// RPC (lens PRD §8, §10). Async by default (prints `{session_id, pane_id,
/// revision}`); `--wait` blocks for completion, adds `exit_code`, and once
/// the child has started, the CLI process itself exits with the CHILD's
/// code — authoritatively, even if it collides with the exit table below
/// (§10's documented precedence rule; scripts needing certainty parse
/// `--format json`, where `exit_code` is present iff the child ran).
#[allow(clippy::too_many_arguments)]
pub async fn handle_lens_run(
    stream: &mut tokio::net::UnixStream,
    argv: &[String],
    size: (u16, u16),
    ttl_ms: u64,
    max_runtime_ms: u64,
    env: &[(String, String)],
    cwd: Option<&std::path::Path>,
    wait: bool,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let env_obj: serde_json::Map<String, serde_json::Value> = env
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
        .collect();
    let mut params = serde_json::json!({
        "argv": argv,
        "cols": size.0,
        "rows": size.1,
        "env": serde_json::Value::Object(env_obj),
        "post_exit_ttl_ms": ttl_ms,
        "max_runtime_ms": max_runtime_ms,
        "wait": wait,
    });
    if let Some(c) = cwd {
        params["cwd"] = serde_json::Value::String(c.display().to_string());
    }

    match rpc_call(stream, "lens.run", params).await {
        Ok(result) => {
            match format {
                OutputFormat::Json => {
                    // §10: the `{result}` envelope, byte-identical to
                    // `shux rpc call` (the frozen lens harness parses this).
                    let envelope = serde_json::json!({ "result": result });
                    println!("{}", serde_json::to_string_pretty(&envelope)?);
                }
                OutputFormat::Text | OutputFormat::Plain => {
                    let session_id = result
                        .get("session_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let pane_id = result.get("pane_id").and_then(|v| v.as_str()).unwrap_or("");
                    let revision = result.get("revision").and_then(|v| v.as_u64()).unwrap_or(0);
                    let exit_code = result.get("exit_code").and_then(|v| v.as_i64());
                    crate::style::print_lens_run(session_id, pane_id, revision, exit_code);
                }
            }
            // §10 precedence: once the child has started (wait=true and the
            // RPC returned normally — spawn already succeeded synchronously
            // per LENS-R-045), the CLI exits with the CHILD's code.
            if wait {
                let exit_code = result
                    .get("exit_code")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(-1);
                std::process::exit(exit_code as i32);
            }
            Ok(())
        }
        Err(RpcClientError::Rpc {
            code,
            message,
            data,
        }) => lens_emit_error_and_exit(
            format,
            "lens run",
            code,
            &message,
            data,
            lens_run_exit_code(code),
        ),
        Err(other) => Err(other.into()),
    }
}

/// `shux pane set-size` — call `pane.set_size` RPC with absolute dims.
pub async fn handle_pane_set_size(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    window_spec: Option<&str>,
    pane_spec: Option<&str>,
    cols: u16,
    rows: u16,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let (session_id, window_id) = resolve_pane_window_id(stream, session_name, window_spec).await?;
    let mut params = serde_json::json!({
        "session_id": session_id,
        "window_id": window_id,
        "cols": cols,
        "rows": rows,
    });
    if let Some(pid) = pane_spec {
        params["pane_id"] = serde_json::Value::String(pid.to_string());
    }

    let result = rpc_call(stream, "pane.set_size", params).await?;

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        OutputFormat::Text | OutputFormat::Plain => {
            println!(
                "{} pane resized to {}×{}",
                crate::style::success("✓"),
                cols,
                rows,
            );
        }
    }
    Ok(())
}

pub async fn handle_pane_capture(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    window_spec: Option<&str>,
    pane_spec: Option<&str>,
    lines: u64,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let (session_id, window_id) = resolve_pane_window_id(stream, session_name, window_spec).await?;

    let mut params = serde_json::json!({
        "session_id": session_id,
        "window_id": window_id,
        "lines": lines,
    });

    if let Some(pid) = pane_spec {
        params["pane_id"] = serde_json::Value::String(pid.to_string());
    }

    let result = rpc_call(stream, "pane.capture", params).await?;

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        OutputFormat::Text | OutputFormat::Plain => {
            let text = result.get("text").and_then(|v| v.as_str()).unwrap_or("");
            print!("{text}");
        }
    }

    Ok(())
}

/// `shux events watch [--filter ...] [--from-seq N] [--limit N]`.
///
/// Long-polls `events.watch` on a single shared connection. Each loop:
///   1. Calls `events.watch` with `from_seq` = next expected seq.
///   2. Prints every event in the response as one JSON Line on stdout.
///   3. Updates `from_seq` from the response's `next_seq`.
///   4. If `lagged: true`, prints `[STREAM_DEGRADED]` to stderr (per the
///      Codex+Gemini review — clients must know the stream dropped events).
///   5. If `gap > 0` on the first call (resumption from too-old `from_seq`),
///      prints `[GAP n]` to stderr.
///   6. Stops when `--limit N` events have been printed, or on Ctrl+C.
pub async fn handle_events_watch(
    stream: &mut tokio::net::UnixStream,
    filter: Vec<String>,
    from_seq: Option<u64>,
    timeout_ms: u64,
    limit: Option<u64>,
) -> anyhow::Result<()> {
    use crate::style;

    let mut next_seq = from_seq;
    let mut printed: u64 = 0;
    let mut first_call = true;

    loop {
        let mut params = serde_json::Map::new();
        if let Some(seq) = next_seq {
            params.insert("from_seq".into(), serde_json::json!(seq));
        }
        if !filter.is_empty() {
            params.insert("filter".into(), serde_json::json!(filter));
        }
        params.insert("timeout_ms".into(), serde_json::json!(timeout_ms));

        let result = match rpc_call(stream, "events.watch", serde_json::Value::Object(params)).await
        {
            Ok(v) => v,
            Err(e) => {
                eprintln!("{} {e}", style::error("✗ events.watch failed:"));
                return Err(anyhow::anyhow!(e));
            }
        };

        let gap = result.get("gap").and_then(|v| v.as_u64()).unwrap_or(0);
        let lagged = result
            .get("lagged")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if first_call && gap > 0 {
            eprintln!(
                "{}",
                style::warning(&format!(
                    "[GAP {gap}] resumed from a sequence older than the daemon's history; events were lost."
                ))
            );
            first_call = false;
        } else {
            first_call = false;
        }

        if lagged {
            eprintln!(
                "{}",
                style::warning(
                    "[STREAM_DEGRADED] subscriber lagged; some events were dropped by the daemon."
                )
            );
        }

        if let Some(events) = result.get("events").and_then(|v| v.as_array()) {
            for ev in events {
                println!("{}", serde_json::to_string(ev)?);
                printed += 1;
                if let Some(n) = limit {
                    if printed >= n {
                        return Ok(());
                    }
                }
            }
        }

        if let Some(ns) = result.get("next_seq").and_then(|v| v.as_u64()) {
            next_seq = Some(ns);
        }

        // Loop unconditionally — long-poll cycles immediately when the prior
        // call returned (Codex + Gemini both warned: do NOT add an artificial
        // sleep here, it just adds latency for no benefit).
    }
}

/// `shux events history [--filter ...] [-n N]`.
pub async fn handle_events_history(
    stream: &mut tokio::net::UnixStream,
    filter: Vec<String>,
    count: u64,
) -> anyhow::Result<()> {
    let mut params = serde_json::Map::new();
    params.insert("count".into(), serde_json::json!(count));
    if !filter.is_empty() {
        params.insert("filter".into(), serde_json::json!(filter));
    }

    let result = rpc_call(stream, "events.history", serde_json::Value::Object(params)).await?;

    if let Some(events) = result.get("events").and_then(|v| v.as_array()) {
        for ev in events {
            println!("{}", serde_json::to_string(ev)?);
        }
    }
    Ok(())
}

/// `shux plugin install <path>` — register a plugin executable
/// with the daemon. Spawns a `plugin.install` RPC and reports the
/// resolved manifest.
/// Resolve the per-install plugin-state root from a starting cwd.
/// Walks up looking for an existing `.shux/` ancestor (so a plugin
/// installed from a subdirectory of a project still lands its state
/// in the project's `.shux/plugins/`). Falls back to anchoring at
/// the cwd itself when no `.shux/` is found in any ancestor.
fn resolve_plugin_state_root(start: &std::path::Path) -> std::path::PathBuf {
    let mut cur = start;
    loop {
        if cur.join(".shux").is_dir() {
            return cur.join(".shux").join("plugins");
        }
        match cur.parent() {
            Some(p) => cur = p,
            None => break,
        }
    }
    start.join(".shux").join("plugins")
}

pub fn handle_plugin_scaffold(
    path: &std::path::Path,
    runtime: PluginScaffoldRuntime,
    name: Option<String>,
    id: Option<String>,
    force: bool,
    format: OutputFormat,
) -> anyhow::Result<()> {
    use crate::features::plugin;
    use crate::style;

    let report = plugin::scaffold_plugin(
        path,
        &ScaffoldOptions {
            runtime,
            name,
            id,
            force,
        },
    )?;

    match format {
        OutputFormat::Json => {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "root": report.root,
                    "name": report.name,
                    "id": report.id,
                    "runtime": runtime.as_str(),
                    "entrypoint": report.entrypoint,
                }))?
            );
        }
        OutputFormat::Plain => {
            println!(
                "{}\t{}\t{}\t{}",
                report.name,
                report.id,
                runtime.as_str(),
                report.root.display()
            );
        }
        OutputFormat::Text => {
            println!(
                "{} {} {}",
                style::success("✓ scaffolded plugin"),
                style::bold(&report.name),
                style::muted(&format!("at {}", report.root.display())),
            );
            println!(
                "  {} {}",
                style::muted("entrypoint"),
                report.entrypoint.display()
            );
        }
    }
    Ok(())
}

pub async fn handle_plugin_install(
    stream: &mut tokio::net::UnixStream,
    path: &std::path::Path,
    args: &[String],
    cwd: Option<&std::path::Path>,
    watch: bool,
    format: OutputFormat,
) -> anyhow::Result<()> {
    use crate::features::plugin;
    use crate::style;

    let resolved = plugin::resolve_plugin_package(path)?;
    let mut resolved_args = resolved.args;
    resolved_args.extend(args.iter().cloned());

    let mut params = serde_json::Map::new();
    params.insert(
        "path".into(),
        serde_json::Value::String(resolved.command.display().to_string()),
    );
    if !resolved_args.is_empty() {
        params.insert("args".into(), serde_json::json!(resolved_args));
    }
    let resolved_cwd = cwd.map(std::path::Path::to_path_buf).or(resolved.cwd);
    if let Some(cwd) = resolved_cwd.as_deref() {
        params.insert(
            "cwd".into(),
            serde_json::Value::String(cwd.display().to_string()),
        );
    }
    if let Some(expected_name) = resolved.expected_name {
        params.insert(
            "expected_name".into(),
            serde_json::Value::String(expected_name),
        );
    }
    if let Some(expected_version) = resolved.expected_version {
        params.insert(
            "expected_version".into(),
            serde_json::Value::String(expected_version),
        );
    }
    params.insert("watch".into(), serde_json::Value::Bool(watch));

    // Pin the plugin's persisted-state root to the CLIENT's cwd so a
    // daemon shared across multiple project checkouts keeps each
    // project's plugin state isolated (codex P2 review on PR #32).
    // Walks up from cwd to find an existing `.shux/` ancestor; if
    // none found, anchors at the cwd itself. The daemon creates the
    // `<state_root>/<plugin_name>/` dir lazily on first `state.set`.
    if let Ok(cwd) = std::env::current_dir() {
        let state_root = resolve_plugin_state_root(&cwd);
        params.insert(
            "state_root".into(),
            serde_json::Value::String(state_root.display().to_string()),
        );
    }

    let result = rpc_call(stream, "plugin.install", serde_json::Value::Object(params)).await?;

    match format {
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&result)?),
        OutputFormat::Plain => {
            let name = result.get("name").and_then(|v| v.as_str()).unwrap_or("?");
            let ver = result
                .get("version")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let pid = result.get("pid").and_then(|v| v.as_u64()).unwrap_or(0);
            println!("{name}\t{ver}\t{pid}");
        }
        OutputFormat::Text => {
            let name = result.get("name").and_then(|v| v.as_str()).unwrap_or("?");
            let ver = result
                .get("version")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let pid = result.get("pid").and_then(|v| v.as_u64()).unwrap_or(0);
            let watching = result
                .get("watching")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let subs: Vec<String> = result
                .get("subscribes")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let sub_str = if subs.is_empty() {
                String::from("∅")
            } else {
                subs.join(",")
            };
            let watch_str = if watching { ", watching" } else { "" };
            // Strip a leading "v" the plugin manifest may have
            // already supplied so we don't end up with "vv1".
            let display_ver = ver.strip_prefix('v').unwrap_or(ver);
            println!(
                "{} {} {} (pid {}, subscribes: {}{})",
                style::success("✓ installed plugin"),
                style::bold(name),
                style::muted(&format!("v{display_ver}")),
                pid,
                style::muted(&sub_str),
                style::muted(watch_str),
            );
        }
    }
    Ok(())
}

/// `shux plugin reload <name>` — manual hot-reload tick. The daemon
/// kills + respawns the plugin from the same source. Equivalent to
/// what the file watcher does automatically when `--no-watch` was
/// not passed.
pub async fn handle_plugin_reload(
    stream: &mut tokio::net::UnixStream,
    name: &str,
    format: OutputFormat,
) -> anyhow::Result<()> {
    use crate::style;

    let params = serde_json::json!({ "name": name });
    let result = rpc_call(stream, "plugin.reload", params).await?;

    match format {
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&result)?),
        OutputFormat::Plain => {
            let pid = result.get("pid").and_then(|v| v.as_u64()).unwrap_or(0);
            println!("{name}\t{pid}");
        }
        OutputFormat::Text => {
            let pid = result.get("pid").and_then(|v| v.as_u64()).unwrap_or(0);
            println!(
                "{} {} (pid {})",
                style::success("✓ reloaded plugin"),
                style::bold(name),
                pid,
            );
        }
    }
    Ok(())
}

/// `shux plugin list` — print every running plugin in a small box.
pub async fn handle_plugin_list(
    stream: &mut tokio::net::UnixStream,
    format: OutputFormat,
) -> anyhow::Result<()> {
    use crate::style;

    let result = rpc_call(stream, "plugin.list", serde_json::json!({})).await?;
    let plugins = result
        .get("plugins")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        OutputFormat::Plain => {
            for p in &plugins {
                let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                let ver = p.get("version").and_then(|v| v.as_str()).unwrap_or("?");
                let status = p.get("status").and_then(|v| v.as_str()).unwrap_or("?");
                let pid = p.get("pid").and_then(|v| v.as_u64()).unwrap_or(0);
                let up_ms = p.get("uptime_ms").and_then(|v| v.as_u64()).unwrap_or(0);
                println!("{name}\t{ver}\t{status}\t{pid}\t{up_ms}");
            }
        }
        OutputFormat::Text => {
            if plugins.is_empty() {
                println!("{}", style::muted("no plugins installed"));
                return Ok(());
            }
            println!("{}", style::muted(&format!("{} plugin(s)", plugins.len())));
            for p in &plugins {
                let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                let ver = p.get("version").and_then(|v| v.as_str()).unwrap_or("?");
                let status = p.get("status").and_then(|v| v.as_str()).unwrap_or("?");
                let pid = p.get("pid").and_then(|v| v.as_u64()).unwrap_or(0);
                let up_ms = p.get("uptime_ms").and_then(|v| v.as_u64()).unwrap_or(0);
                let up_s = up_ms / 1000;
                let dot = if status == "running" {
                    style::success("●").to_string()
                } else {
                    style::warning("○").to_string()
                };
                println!(
                    "  {} {} {} {} (pid {}, up {}s)",
                    dot,
                    style::bold(name),
                    style::muted(&format!("v{ver}")),
                    style::muted(status),
                    pid,
                    up_s
                );
            }
        }
    }
    Ok(())
}

/// `shux plugin kill <name>` — send shutdown + reap.
pub async fn handle_plugin_kill(
    stream: &mut tokio::net::UnixStream,
    name: &str,
    format: OutputFormat,
) -> anyhow::Result<()> {
    use crate::style;

    let params = serde_json::json!({ "name": name });
    let result = rpc_call(stream, "plugin.kill", params).await?;

    match format {
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&result)?),
        OutputFormat::Plain => println!("{name}\tkilled"),
        OutputFormat::Text => println!(
            "{} {}",
            style::success("✓ killed plugin"),
            style::bold(name)
        ),
    }
    Ok(())
}

/// `shux plugin stop <name>` — UX alias for graceful shutdown + unregister.
pub async fn handle_plugin_stop(
    stream: &mut tokio::net::UnixStream,
    name: &str,
    format: OutputFormat,
) -> anyhow::Result<()> {
    use crate::style;

    let params = serde_json::json!({ "name": name });
    let result = rpc_call(stream, "plugin.kill", params).await?;

    match format {
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&result)?),
        OutputFormat::Plain => println!("{name}\tstopped"),
        OutputFormat::Text => println!(
            "{} {}",
            style::success("✓ stopped plugin"),
            style::bold(name)
        ),
    }
    Ok(())
}

pub async fn handle_plugin_grant(
    stream: &mut tokio::net::UnixStream,
    plugin: &str,
    method: &str,
    target: Option<&str>,
    subscribe: bool,
    format: OutputFormat,
) -> anyhow::Result<()> {
    use crate::style;

    let mut params = serde_json::Map::new();
    params.insert("plugin".into(), plugin.into());
    params.insert("method".into(), method.into());
    if let Some(t) = target {
        params.insert("target".into(), t.into());
    }
    if subscribe {
        params.insert("subscribe".into(), true.into());
    }
    let result = rpc_call(stream, "plugin.grant", serde_json::Value::Object(params)).await?;
    match format {
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&result)?),
        OutputFormat::Plain => {
            let scope = target.unwrap_or("*");
            let kind = if subscribe { "subscribe" } else { "method" };
            println!("{plugin}\t{kind}\t{method}\t{scope}\tgranted");
        }
        OutputFormat::Text => {
            let scope = target.map(|t| format!(" → {t}")).unwrap_or_default();
            let kind = if subscribe { " (subscribe)" } else { "" };
            println!(
                "{} {} {} {}{}",
                style::success("✓ granted"),
                style::bold(plugin),
                style::accent(method),
                style::muted(&scope),
                kind
            );
        }
    }
    Ok(())
}

pub async fn handle_plugin_revoke(
    stream: &mut tokio::net::UnixStream,
    plugin: &str,
    method: &str,
    target: Option<&str>,
    subscribe: bool,
    format: OutputFormat,
) -> anyhow::Result<()> {
    use crate::style;

    let mut params = serde_json::Map::new();
    params.insert("plugin".into(), plugin.into());
    params.insert("method".into(), method.into());
    if let Some(t) = target {
        params.insert("target".into(), t.into());
    }
    if subscribe {
        params.insert("subscribe".into(), true.into());
    }
    let result = rpc_call(stream, "plugin.revoke", serde_json::Value::Object(params)).await?;
    match format {
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&result)?),
        OutputFormat::Plain => {
            let scope = target.unwrap_or("*");
            let kind = if subscribe { "subscribe" } else { "method" };
            println!("{plugin}\t{kind}\t{method}\t{scope}\trevoked");
        }
        OutputFormat::Text => {
            let scope = target.map(|t| format!(" → {t}")).unwrap_or_default();
            let kind = if subscribe { " (subscribe)" } else { "" };
            println!(
                "{} {} {} {}{}",
                style::success("✓ revoked"),
                style::bold(plugin),
                style::accent(method),
                style::muted(&scope),
                kind
            );
        }
    }
    Ok(())
}

pub async fn handle_plugin_grants(
    stream: &mut tokio::net::UnixStream,
    plugin: &str,
    format: OutputFormat,
) -> anyhow::Result<()> {
    use crate::style;

    let params = serde_json::json!({"plugin": plugin});
    let result = rpc_call(stream, "plugin.grants", params).await?;
    match format {
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&result)?),
        OutputFormat::Plain => {
            if let Some(g) = result.get("grants").and_then(|v| v.as_object()) {
                for (method, scope) in g {
                    let scope_str = match scope {
                        serde_json::Value::String(s) => s.clone(),
                        serde_json::Value::Array(a) => a
                            .iter()
                            .filter_map(|v| v.as_str())
                            .collect::<Vec<_>>()
                            .join(","),
                        _ => "?".into(),
                    };
                    println!("grant\t{method}\t{scope_str}");
                }
            }
            if let Some(allowed) = result
                .get("subscribes")
                .and_then(|s| s.get("allowed"))
                .and_then(|a| a.as_array())
            {
                for f in allowed.iter().filter_map(|v| v.as_str()) {
                    println!("subscribe\t{f}");
                }
            }
        }
        OutputFormat::Text => {
            println!("{} {}", style::accent("plugin"), style::bold(plugin));
            let g_map = result.get("grants").and_then(|v| v.as_object());
            let empty = g_map.map(|m| m.is_empty()).unwrap_or(true);
            if empty {
                println!("  {}", style::muted("(no grants)"));
            } else if let Some(g) = g_map {
                println!("  {}", style::bold("methods:"));
                for (method, scope) in g {
                    let scope_str = match scope {
                        serde_json::Value::String(s) => s.clone(),
                        serde_json::Value::Array(a) => format!(
                            "[{}]",
                            a.iter()
                                .filter_map(|v| v.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                        _ => "?".into(),
                    };
                    println!(
                        "    {} → {}",
                        style::accent(method),
                        style::muted(&scope_str)
                    );
                }
            }
            if let Some(allowed) = result
                .get("subscribes")
                .and_then(|s| s.get("allowed"))
                .and_then(|a| a.as_array())
                && !allowed.is_empty()
            {
                println!("  {}", style::bold("subscribes:"));
                for f in allowed.iter().filter_map(|v| v.as_str()) {
                    println!("    {}", style::accent(f));
                }
            }
        }
    }
    Ok(())
}

pub async fn handle_plugin_audit(
    stream: &mut tokio::net::UnixStream,
    plugin: &str,
    tail: usize,
    format: OutputFormat,
) -> anyhow::Result<()> {
    use crate::style;

    let params = serde_json::json!({"plugin": plugin, "tail": tail});
    let result = rpc_call(stream, "plugin.audit", params).await?;
    match format {
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&result)?),
        OutputFormat::Plain => {
            if let Some(entries) = result.get("entries").and_then(|v| v.as_array()) {
                for e in entries {
                    println!("{}", serde_json::to_string(e)?);
                }
            }
        }
        OutputFormat::Text => {
            let path = result
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("<unknown>");
            println!("{} {}", style::muted("audit log:"), style::muted(path));
            let entries = result
                .get("entries")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            if entries.is_empty() {
                println!("  {}", style::muted("(empty)"));
            }
            for e in entries {
                let ts = e.get("ts").and_then(|v| v.as_str()).unwrap_or("?");
                let m = e.get("method").and_then(|v| v.as_str()).unwrap_or("?");
                let d = e.get("decision").and_then(|v| v.as_str()).unwrap_or("?");
                let r = e.get("reason").and_then(|v| v.as_str()).unwrap_or("?");
                let stamp = style::muted(ts);
                let method = style::accent(m);
                let decision = if d == "allow" {
                    style::success(d).to_string()
                } else {
                    style::error(d).to_string()
                };
                println!("  {} {} {} {}", stamp, decision, method, style::muted(r));
            }
        }
    }
    Ok(())
}

/// `shux apply <template.toml>` — send the lowered ops to `state.apply`,
/// pretty-print the result, optionally hand off to `events watch` filtered
/// to the new session.
pub async fn handle_apply(
    stream: &mut tokio::net::UnixStream,
    ops: Vec<shux_core::apply::Op>,
    watch: bool,
    socket_path: &std::path::Path,
) -> anyhow::Result<()> {
    use crate::style;

    let params = serde_json::json!({ "ops": ops });
    let result = match rpc_call(stream, "state.apply", params).await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{} {e}", style::error("✗ apply failed:"));
            return Err(anyhow::anyhow!(e));
        }
    };

    // Summarize result for humans. correlation_id + counts on the first
    // line; per-pane spawn rows below.
    let cid = result
        .get("correlation_id")
        .and_then(|v| v.as_str())
        .unwrap_or("?");
    let outputs = result
        .get("outputs")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    let last_seq = result
        .get("last_event_seq")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let spawns: Vec<_> = result
        .get("spawn_results")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let spawned_ok = spawns.iter().filter(|s| s["spawned"] == true).count();
    let spawned_fail = spawns.len() - spawned_ok;

    println!(
        "{} ({} ops, {} panes spawned{}, last event seq {})",
        style::success(&format!("✓ Applied {cid}")),
        outputs,
        spawned_ok,
        if spawned_fail > 0 {
            format!(", {spawned_fail} failed")
        } else {
            String::new()
        },
        last_seq
    );
    for s in &spawns {
        let pid = s["pane_id"].as_str().unwrap_or("?");
        let pid_short: String = pid.chars().take(8).collect();
        if s["spawned"] == true {
            println!("    {} pane {} spawned", style::success("✓"), pid_short);
        } else {
            let err = s["error"].as_str().unwrap_or("unknown error");
            println!(
                "    {} pane {} spawn failed: {}",
                style::error("✗"),
                pid_short,
                err
            );
        }
    }

    if watch {
        use crate::client;
        // Resolve the new session_id from the first output and start an
        // events.watch loop scoped to it.
        let session_id = result
            .get("outputs")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .and_then(|first| first.get("session_id"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        if let Some(_sid) = session_id {
            println!(
                "\n{}",
                style::muted(&format!(
                    "Streaming events for the new session (resume from seq {} +1)…",
                    last_seq
                ))
            );
            let mut stream2 = client::ensure_daemon_running_at(socket_path).await?;
            // Filter on the correlation_id by re-reading session events; in
            // a future PR we can add a server-side --correlation-id filter
            // for events.watch. For now: tail all events from last_seq+1 and
            // let the user Ctrl+C when they've seen enough.
            handle_events_watch(&mut stream2, vec![], Some(last_seq + 1), 5_000, None).await?;
        }
    }

    Ok(())
}

/// Handle the `shux version` command.
pub async fn handle_version(
    stream: &mut tokio::net::UnixStream,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let result = rpc_call(
        stream,
        "system.version",
        serde_json::Value::Object(Default::default()),
    )
    .await?;

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        OutputFormat::Text | OutputFormat::Plain => {
            let version = result
                .get("version")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let git_sha = result.get("git_sha").and_then(|v| v.as_str());
            crate::style::print_version(version, git_sha, None);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex as StdMutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn spawn_rpc_script(
        responses: Vec<serde_json::Value>,
    ) -> (
        tokio::net::UnixStream,
        Arc<StdMutex<Vec<serde_json::Value>>>,
        tokio::task::JoinHandle<()>,
    ) {
        let (client, mut server) = tokio::net::UnixStream::pair().unwrap();
        let requests = Arc::new(StdMutex::new(Vec::new()));
        let captured = requests.clone();
        let task = tokio::spawn(async move {
            for scripted in responses {
                let mut len_buf = [0u8; 4];
                server.read_exact(&mut len_buf).await.unwrap();
                let len = u32::from_be_bytes(len_buf) as usize;
                let mut payload = vec![0u8; len];
                server.read_exact(&mut payload).await.unwrap();
                let request: serde_json::Value = serde_json::from_slice(&payload).unwrap();
                captured.lock().unwrap().push(request.clone());

                let response = if let Some(error) = scripted.get("error").filter(|e| !e.is_null()) {
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": request.get("id").cloned().unwrap_or(serde_json::Value::Null),
                        "error": error,
                    })
                } else {
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": request.get("id").cloned().unwrap_or(serde_json::Value::Null),
                        "result": scripted,
                    })
                };
                let bytes = serde_json::to_vec(&response).unwrap();
                server
                    .write_all(&(bytes.len() as u32).to_be_bytes())
                    .await
                    .unwrap();
                server.write_all(&bytes).await.unwrap();
                server.flush().await.unwrap();
            }
        });
        (client, requests, task)
    }

    async fn finish_rpc_script(
        client: tokio::net::UnixStream,
        task: tokio::task::JoinHandle<()>,
        requests: Arc<StdMutex<Vec<serde_json::Value>>>,
    ) -> Vec<serde_json::Value> {
        drop(client);
        task.await.unwrap();
        Arc::try_unwrap(requests).unwrap().into_inner().unwrap()
    }

    fn session_list_response(session_id: &str, window_id: &str) -> serde_json::Value {
        serde_json::json!({
            "sessions": [{
                "id": session_id,
                "name": "dev",
                "active_window_id": window_id,
                "windows": [window_id],
                "window_count": 1,
                "created_at": 0
            }]
        })
    }

    fn window_list_response(window_id: &str, pane_id: &str) -> serde_json::Value {
        serde_json::json!([{
            "id": window_id,
            "title": "main",
            "index": 0,
            "pane_count": 1,
            "active_pane_id": pane_id,
            "is_active": true,
            "version": 7
        }])
    }

    #[test]
    fn test_socket_path_explicit() {
        let cli = Cli {
            command: None,
            format: OutputFormat::Text,
            socket: Some(PathBuf::from("/custom/path.sock")),
            verbose: false,
        };
        assert_eq!(cli.socket_path(), PathBuf::from("/custom/path.sock"));
    }

    #[test]
    fn test_socket_path_fallback() {
        let cli = Cli {
            command: None,
            format: OutputFormat::Text,
            socket: None,
            verbose: false,
        };
        let path = cli.socket_path();

        // Should end with shux.sock
        assert!(
            path.to_string_lossy().ends_with("shux.sock"),
            "socket path should end with shux.sock, got: {}",
            path.display()
        );

        // Should be an absolute path
        assert!(path.is_absolute());
    }

    #[test]
    fn test_output_format_default() {
        let format = OutputFormat::default();
        assert!(matches!(format, OutputFormat::Text));
    }

    // ───── Session namespace ─────
    //
    // Top-level `shux new/ls/kill/rename/attach` was removed in
    // the May 2026 CLI consistency overhaul. Codex council
    // verdict: RPC dots become CLI spaces, no top-level shortcut
    // verbs. Every session op now lives under `shux session`.

    #[test]
    fn test_cli_parse_session_list() {
        let cli = Cli::try_parse_from(["shux", "session", "list"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Session {
                command: SessionCommand::List {
                    include_scratch: false
                }
            })
        ));
    }

    #[test]
    fn test_cli_parse_session_list_alias_ls() {
        let cli = Cli::try_parse_from(["shux", "session", "ls"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Session {
                command: SessionCommand::List {
                    include_scratch: false
                }
            })
        ));
    }

    #[test]
    fn test_cli_parse_session_create_with_options() {
        let cli =
            Cli::try_parse_from(["shux", "session", "create", "-s", "work", "-d", "--ensure"])
                .unwrap();
        match cli.command {
            Some(Command::Session {
                command:
                    SessionCommand::Create {
                        name,
                        session,
                        ensure,
                        detached,
                        cwd,
                        title,
                        cmd,
                        argv,
                    },
            }) => {
                assert!(name.is_none());
                assert_eq!(session, Some("work".to_string()));
                assert!(ensure);
                assert!(detached);
                assert!(cwd.is_none());
                assert!(title.is_none());
                assert!(cmd.is_none());
                assert!(argv.is_empty());
            }
            _ => panic!("expected session create command"),
        }
    }

    #[test]
    fn test_cli_parse_session_create_cwd() {
        let cli = Cli::try_parse_from(["shux", "session", "create", "work", "--cwd", "/tmp/demo"])
            .unwrap();
        match cli.command {
            Some(Command::Session {
                command: SessionCommand::Create { cwd, .. },
            }) => {
                assert_eq!(cwd, Some(std::path::PathBuf::from("/tmp/demo")));
            }
            _ => panic!("expected session create command"),
        }
    }

    #[test]
    fn test_cli_parse_session_create_title() {
        let cli = Cli::try_parse_from(["shux", "session", "create", "work", "--title", "agent"]);
        let cli = cli.unwrap();
        match cli.command {
            Some(Command::Session {
                command: SessionCommand::Create { title, .. },
            }) => {
                assert_eq!(title, Some("agent".to_string()));
            }
            _ => panic!("expected session create command"),
        }
    }

    /// `shux session create <NAME>` — positional NAME parses into
    /// the dedicated `name` field, not `--session`.
    #[test]
    fn test_cli_parse_session_create_positional_name() {
        let cli = Cli::try_parse_from(["shux", "session", "create", "work"]).unwrap();
        match cli.command {
            Some(Command::Session {
                command: SessionCommand::Create { name, session, .. },
            }) => {
                assert_eq!(name, Some("work".to_string()));
                assert!(session.is_none(), "flag form should remain empty");
            }
            _ => panic!("expected session create command"),
        }
    }

    /// Trailing argv after `--` lands on `argv`.
    #[test]
    fn test_cli_parse_session_create_trailing_argv() {
        let cli = Cli::try_parse_from([
            "shux", "session", "create", "-s", "vim", "--", "vim", "foo.rs",
        ])
        .unwrap();
        match cli.command {
            Some(Command::Session {
                command: SessionCommand::Create { session, argv, .. },
            }) => {
                assert_eq!(session, Some("vim".to_string()));
                assert_eq!(argv, vec!["vim".to_string(), "foo.rs".to_string()]);
            }
            _ => panic!("expected session create command"),
        }
    }

    #[test]
    fn test_resolve_session_create_cwd_defaults_to_invocation_cwd() {
        let cwd = resolve_session_create_cwd(None, std::path::Path::new("/tmp/shux-demo"));

        assert_eq!(cwd, std::path::PathBuf::from("/tmp/shux-demo"));
    }

    #[test]
    fn test_resolve_session_create_cwd_absolutizes_relative_override() {
        let cwd = resolve_session_create_cwd(
            Some(std::path::PathBuf::from("nested/project")),
            std::path::Path::new("/tmp/shux-demo"),
        );

        assert_eq!(
            cwd,
            std::path::PathBuf::from("/tmp/shux-demo/nested/project")
        );
    }

    #[test]
    fn test_resolve_session_create_cwd_preserves_absolute_override() {
        let cwd = resolve_session_create_cwd(
            Some(std::path::PathBuf::from("/var/tmp/shux-project")),
            std::path::Path::new("/tmp/shux-demo"),
        );

        assert_eq!(cwd, std::path::PathBuf::from("/var/tmp/shux-project"));
    }

    #[test]
    fn test_build_session_create_params_always_includes_cwd() {
        let params = build_session_create_params(
            Some("demo".to_string()),
            std::path::PathBuf::from("/tmp/shux-demo"),
            Some("aww-shux".to_string()),
            None,
            vec!["pwd".to_string()],
        );

        assert_eq!(params.get("name").and_then(|v| v.as_str()), Some("demo"));
        assert_eq!(
            params.get("cwd").and_then(|v| v.as_str()),
            Some("/tmp/shux-demo")
        );
        assert_eq!(
            params.get("pane_title").and_then(|v| v.as_str()),
            Some("aww-shux")
        );
        assert_eq!(params.get("command"), Some(&serde_json::json!(["pwd"])));
    }

    #[test]
    fn test_agent_help_raw_rpc_cwd_example_is_copy_safe() {
        let help = render_agent_help(false);

        assert!(
            help.contains(r#"--params "{\"name\":\"demo\",\"cwd\":\"$(pwd)\","#),
            "raw RPC cwd example should use shell-expanded $(pwd) in double-quoted JSON"
        );
        assert!(
            !help.contains(r#""cwd":"$PWD""#),
            "single-quoted inline JSON would send literal $PWD"
        );
    }

    #[test]
    fn test_cli_parse_session_kill() {
        let cli = Cli::try_parse_from(["shux", "session", "kill", "-s", "mytest"]).unwrap();
        match cli.command {
            Some(Command::Session {
                command:
                    SessionCommand::Kill {
                        session, name_pos, ..
                    },
            }) => {
                assert_eq!(session, Some("mytest".to_string()));
                assert!(name_pos.is_none());
            }
            _ => panic!("expected session kill command"),
        }
    }

    /// Positional NAME on `session kill` (mirrors `session create`).
    #[test]
    fn test_cli_parse_session_kill_positional() {
        let cli = Cli::try_parse_from(["shux", "session", "kill", "mytest"]).unwrap();
        match cli.command {
            Some(Command::Session {
                command: SessionCommand::Kill { name_pos, .. },
            }) => {
                assert_eq!(name_pos, Some("mytest".to_string()));
            }
            _ => panic!("expected session kill command"),
        }
    }

    #[test]
    fn test_cli_parse_session_rename() {
        let cli =
            Cli::try_parse_from(["shux", "session", "rename", "-s", "old", "-n", "new"]).unwrap();
        match cli.command {
            Some(Command::Session {
                command: SessionCommand::Rename { session, name, .. },
            }) => {
                assert_eq!(session, "old");
                assert_eq!(name, "new");
            }
            _ => panic!("expected session rename command"),
        }
    }

    #[test]
    fn test_cli_parse_session_attach_positional() {
        let cli = Cli::try_parse_from(["shux", "session", "attach", "dev"]).unwrap();
        match cli.command {
            Some(Command::Session {
                command: SessionCommand::Attach { name_pos, session },
            }) => {
                assert_eq!(name_pos, Some("dev".to_string()));
                assert!(session.is_none());
            }
            _ => panic!("expected session attach command"),
        }
    }

    #[test]
    fn test_cli_parse_session_save() {
        let cli = Cli::try_parse_from([
            "shux",
            "session",
            "save",
            "-s",
            "dev",
            "-o",
            ".shux/templates/dev.toml",
        ])
        .unwrap();
        match cli.command {
            Some(Command::Session {
                command: SessionCommand::Save { session, output },
            }) => {
                assert_eq!(session, "dev");
                assert_eq!(
                    output,
                    Some(std::path::PathBuf::from(".shux/templates/dev.toml"))
                );
            }
            _ => panic!("expected session save command"),
        }
    }

    #[test]
    fn test_cli_parse_session_restore_dry_run() {
        let cli = Cli::try_parse_from([
            "shux",
            "session",
            "restore",
            ".shux/templates/dev.toml",
            "--dry-run",
        ])
        .unwrap();
        match cli.command {
            Some(Command::Session {
                command:
                    SessionCommand::Restore {
                        template,
                        dry_run,
                        watch,
                    },
            }) => {
                assert_eq!(
                    template,
                    std::path::PathBuf::from(".shux/templates/dev.toml")
                );
                assert!(dry_run);
                assert!(!watch);
            }
            _ => panic!("expected session restore command"),
        }
    }

    /// Session aliases `ses` and `sess` parse identically.
    #[test]
    fn test_cli_parse_session_alias() {
        let cli = Cli::try_parse_from(["shux", "ses", "list"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Session {
                command: SessionCommand::List {
                    include_scratch: false
                }
            })
        ));
        let cli = Cli::try_parse_from(["shux", "sess", "list"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Session {
                command: SessionCommand::List {
                    include_scratch: false
                }
            })
        ));
    }

    /// Old top-level forms are gone. Make sure they fail loudly
    /// (clap will return an error, not silently match something).
    #[test]
    fn test_cli_old_top_level_verbs_rejected() {
        for old in [
            "new", "ls", "list", "kill", "rename", "attach", "api", "apply",
        ] {
            let result = Cli::try_parse_from(["shux", old]);
            assert!(
                result.is_err(),
                "old top-level `shux {old}` should error after CLI overhaul"
            );
        }
    }

    // ───── RPC namespace (replaces top-level `api`) ─────

    #[test]
    fn test_cli_parse_rpc_call() {
        let cli = Cli::try_parse_from([
            "shux",
            "rpc",
            "call",
            "system.version",
            "--params",
            r#"{"key":"val"}"#,
        ])
        .unwrap();
        match cli.command {
            Some(Command::Rpc {
                command: RpcCommand::Call { method, params },
            }) => {
                assert_eq!(method, "system.version");
                assert_eq!(params, r#"{"key":"val"}"#);
            }
            _ => panic!("expected rpc call command"),
        }
    }

    /// `shux rpc call <method>` — no `--params` defaults to `{}`.
    #[test]
    fn test_cli_parse_rpc_call_default_params() {
        let cli = Cli::try_parse_from(["shux", "rpc", "call", "system.health"]).unwrap();
        match cli.command {
            Some(Command::Rpc {
                command: RpcCommand::Call { params, .. },
            }) => {
                assert_eq!(params, "{}");
            }
            _ => panic!("expected rpc call command"),
        }
    }

    /// `--params @file` and `--params -` should parse as their
    /// literal strings (resolved at dispatch time, not at parse).
    #[test]
    fn test_cli_parse_rpc_call_params_file_or_stdin() {
        let cli = Cli::try_parse_from([
            "shux",
            "rpc",
            "call",
            "session.create",
            "--params",
            "@/tmp/p.json",
        ])
        .unwrap();
        match cli.command {
            Some(Command::Rpc {
                command: RpcCommand::Call { params, .. },
            }) => {
                assert_eq!(params, "@/tmp/p.json");
            }
            _ => panic!("expected rpc call command"),
        }

        let cli = Cli::try_parse_from(["shux", "rpc", "call", "session.create", "--params", "-"])
            .unwrap();
        match cli.command {
            Some(Command::Rpc {
                command: RpcCommand::Call { params, .. },
            }) => {
                assert_eq!(params, "-");
            }
            _ => panic!("expected rpc call command"),
        }
    }

    // ───── State namespace (replaces top-level `apply`) ─────

    #[test]
    fn test_cli_parse_state_apply() {
        let cli = Cli::try_parse_from(["shux", "state", "apply", "./spec.toml"]).unwrap();
        match cli.command {
            Some(Command::State {
                command:
                    StateCommand::Apply {
                        template,
                        dry_run,
                        watch,
                    },
            }) => {
                assert_eq!(template, std::path::PathBuf::from("./spec.toml"));
                assert!(!dry_run);
                assert!(!watch);
            }
            _ => panic!("expected state apply command"),
        }
    }

    // ───── Global flags + edge cases ─────

    #[test]
    fn test_cli_parse_global_format() {
        let cli = Cli::try_parse_from(["shux", "--format", "json", "session", "list"]).unwrap();
        assert!(matches!(cli.format, OutputFormat::Json));
    }

    #[test]
    fn test_cli_parse_format_plain() {
        let cli = Cli::try_parse_from(["shux", "--format", "plain", "session", "list"]).unwrap();
        assert!(matches!(cli.format, OutputFormat::Plain));
    }

    #[test]
    fn test_cli_parse_global_socket() {
        let cli =
            Cli::try_parse_from(["shux", "--socket", "/tmp/my.sock", "session", "list"]).unwrap();
        assert_eq!(cli.socket, Some(PathBuf::from("/tmp/my.sock")));
    }

    #[test]
    fn test_cli_parse_verbose() {
        let cli = Cli::try_parse_from(["shux", "-v", "session", "list"]).unwrap();
        assert!(cli.verbose);
    }

    #[test]
    fn test_cli_no_subcommand() {
        let cli = Cli::try_parse_from(["shux"]).unwrap();
        assert!(cli.command.is_none());
    }

    #[test]
    fn test_cli_version_subcommand() {
        let cli = Cli::try_parse_from(["shux", "version"]).unwrap();
        assert!(matches!(cli.command, Some(Command::Version)));
    }

    #[test]
    fn test_cli_session_rename_requires_both_args() {
        let result = Cli::try_parse_from(["shux", "session", "rename", "-s", "old"]);
        assert!(result.is_err());

        let result = Cli::try_parse_from(["shux", "session", "rename", "-n", "new"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_cli_window_list() {
        let cli = Cli::try_parse_from(["shux", "window", "list", "-s", "work"]).unwrap();
        match cli.command {
            Some(Command::Window {
                command: WindowCommand::List { session },
            }) => {
                assert_eq!(session, "work");
            }
            _ => panic!("expected Window List command"),
        }
    }

    #[test]
    fn test_cli_window_list_alias() {
        let cli = Cli::try_parse_from(["shux", "window", "ls", "-s", "work"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Window {
                command: WindowCommand::List { .. }
            })
        ));
    }

    #[test]
    fn test_cli_window_alias() {
        let cli = Cli::try_parse_from(["shux", "win", "list", "-s", "work"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Window {
                command: WindowCommand::List { .. }
            })
        ));
    }

    #[test]
    fn test_cli_window_new() {
        let cli = Cli::try_parse_from(["shux", "window", "create", "-s", "work", "-n", "editor"])
            .unwrap();
        match cli.command {
            Some(Command::Window {
                command:
                    WindowCommand::Create {
                        session,
                        name,
                        cwd,
                        cmd,
                        ensure,
                        argv,
                    },
            }) => {
                assert_eq!(session, "work");
                assert_eq!(name, Some("editor".to_string()));
                assert!(cwd.is_none());
                assert!(cmd.is_none());
                assert!(!ensure);
                assert!(argv.is_empty());
            }
            _ => panic!("expected Window New command"),
        }
    }

    /// `shux window new -s X -n Y --cwd /tmp --cmd "vim foo"` exposes
    /// every RPC param `window.create` accepts. Codex v3 dogfood:
    /// CLI --help hid these and forced prototyping via `shux rpc call`.
    #[test]
    fn test_cli_window_new_cwd_and_cmd() {
        let cli = Cli::try_parse_from([
            "shux", "window", "create", "-s", "work", "-n", "editor", "--cwd", "/tmp", "--cmd",
            "vim foo",
        ])
        .unwrap();
        match cli.command {
            Some(Command::Window {
                command: WindowCommand::Create { cwd, cmd, argv, .. },
            }) => {
                assert_eq!(cwd, Some(std::path::PathBuf::from("/tmp")));
                assert_eq!(cmd, Some("vim foo".to_string()));
                assert!(argv.is_empty());
            }
            _ => panic!("expected Window New command"),
        }
    }

    /// Trailing argv after `--` lands on `argv` and takes precedence
    /// over `--cmd` (matches `shux session create` behavior).
    #[test]
    fn test_cli_window_new_trailing_argv() {
        let cli = Cli::try_parse_from([
            "shux", "window", "create", "-s", "work", "-n", "editor", "--", "vim", "foo.rs",
        ])
        .unwrap();
        match cli.command {
            Some(Command::Window {
                command: WindowCommand::Create { argv, .. },
            }) => {
                assert_eq!(argv, vec!["vim".to_string(), "foo.rs".to_string()]);
            }
            _ => panic!("expected Window New command"),
        }
    }

    #[test]
    fn test_cli_window_new_ensure() {
        let cli =
            Cli::try_parse_from(["shux", "window", "create", "-s", "work", "--ensure"]).unwrap();
        match cli.command {
            Some(Command::Window {
                command: WindowCommand::Create { ensure, .. },
            }) => {
                assert!(ensure);
            }
            _ => panic!("expected Window New command"),
        }
    }

    #[test]
    fn test_cli_window_kill() {
        let cli =
            Cli::try_parse_from(["shux", "window", "kill", "-s", "work", "-w", "editor"]).unwrap();
        match cli.command {
            Some(Command::Window {
                command:
                    WindowCommand::Kill {
                        session, window, ..
                    },
            }) => {
                assert_eq!(session, "work");
                assert_eq!(window, "editor");
            }
            _ => panic!("expected Window Kill command"),
        }
    }

    #[test]
    fn test_cli_window_rename() {
        let cli = Cli::try_parse_from([
            "shux", "window", "rename", "-s", "work", "-w", "old", "-n", "new",
        ])
        .unwrap();
        match cli.command {
            Some(Command::Window {
                command:
                    WindowCommand::Rename {
                        session,
                        window,
                        name,
                        ..
                    },
            }) => {
                assert_eq!(session, "work");
                assert_eq!(window, "old");
                assert_eq!(name, "new");
            }
            _ => panic!("expected Window Rename command"),
        }
    }

    #[test]
    fn test_cli_window_focus() {
        let cli =
            Cli::try_parse_from(["shux", "window", "focus", "-s", "work", "-w", "0"]).unwrap();
        match cli.command {
            Some(Command::Window {
                command:
                    WindowCommand::Focus {
                        session, window, ..
                    },
            }) => {
                assert_eq!(session, "work");
                assert_eq!(window, "0");
            }
            _ => panic!("expected Window Focus command"),
        }
    }

    #[test]
    fn test_cli_window_reorder() {
        let cli = Cli::try_parse_from([
            "shux", "window", "reorder", "-s", "work", "-w", "editor", "-i", "2",
        ])
        .unwrap();
        match cli.command {
            Some(Command::Window {
                command:
                    WindowCommand::Reorder {
                        session,
                        window,
                        index,
                        ..
                    },
            }) => {
                assert_eq!(session, "work");
                assert_eq!(window, "editor");
                assert_eq!(index, 2);
            }
            _ => panic!("expected Window Reorder command"),
        }
    }

    // Codex hit this in May 2026: `shux pane wait-for --text --search` failed
    // because clap saw `--search` as a flag. Agents matching CLI help output
    // (or any `--`-prefixed needle) shouldn't have to know to write
    // `--text=--search`. Same applies to `--regex` and `pane send-keys`.
    #[test]
    fn test_cli_wait_for_text_accepts_hyphen_value() {
        let cli = Cli::try_parse_from([
            "shux", "pane", "wait-for", "-s", "work", "--text", "--search",
        ])
        .expect("--text should accept a value beginning with --");
        match cli.command {
            Some(Command::Pane {
                command: PaneCommand::WaitFor { text, .. },
            }) => assert_eq!(text.as_deref(), Some("--search")),
            _ => panic!("expected Pane WaitFor command"),
        }
    }

    #[test]
    fn test_cli_wait_for_regex_accepts_hyphen_value() {
        let cli = Cli::try_parse_from([
            "shux",
            "pane",
            "wait-for",
            "-s",
            "work",
            "--regex",
            "--help\\b",
        ])
        .expect("--regex should accept a value beginning with --");
        match cli.command {
            Some(Command::Pane {
                command: PaneCommand::WaitFor { regex, .. },
            }) => assert_eq!(regex.as_deref(), Some("--help\\b")),
            _ => panic!("expected Pane WaitFor command"),
        }
    }

    #[test]
    fn test_cli_send_keys_text_accepts_hyphen_value() {
        let cli = Cli::try_parse_from([
            "shux",
            "pane",
            "send-keys",
            "-s",
            "work",
            "--text",
            "--help",
        ])
        .expect("send-keys --text should accept a value beginning with --");
        match cli.command {
            Some(Command::Pane {
                command: PaneCommand::SendKeys { text, .. },
            }) => assert_eq!(text.as_deref(), Some("--help")),
            _ => panic!("expected Pane SendKeys command"),
        }
    }

    #[test]
    fn test_cli_pane_record_parse() {
        let cli = Cli::try_parse_from([
            "shux",
            "pane",
            "record",
            "-s",
            "work",
            "-p",
            "11111111-1111-4111-8111-111111111111",
            "--to",
            "out.bin",
            "--duration-ms",
            "250",
            "--force",
        ])
        .unwrap();
        match cli.command {
            Some(Command::Pane {
                command:
                    PaneCommand::Record {
                        session,
                        pane,
                        to,
                        force,
                        duration_ms,
                    },
            }) => {
                assert_eq!(session, "work");
                assert_eq!(pane, "11111111-1111-4111-8111-111111111111");
                assert_eq!(to, std::path::PathBuf::from("out.bin"));
                assert!(force);
                assert_eq!(duration_ms, Some(250));
            }
            _ => panic!("expected Pane Record command"),
        }
    }

    #[tokio::test]
    async fn cli_session_handlers_emit_expected_rpc_shapes() {
        let sid = "11111111-1111-4111-8111-111111111111";
        let wid = "22222222-2222-4222-8222-222222222222";
        let template = "[session]\nname = \"dev\"\n";
        let (mut client, requests, task) = spawn_rpc_script(vec![
            session_list_response(sid, wid),
            serde_json::json!({"id": sid, "name": "dev", "created": true}),
            serde_json::json!({"killed": "dev"}),
            serde_json::json!({"id": sid, "name": "renamed"}),
            serde_json::json!({"template": template}),
            serde_json::json!({"version": "0.26.0", "git_sha": "abc123"}),
        ]);

        handle_ls(&mut client, false, OutputFormat::Json)
            .await
            .unwrap();
        let created = handle_new(
            &mut client,
            SessionCreateOptions {
                session_name: Some("dev".to_string()),
                cwd: Some(std::path::PathBuf::from("relative")),
                title: Some("agent".to_string()),
                cmd: Some("ignored".to_string()),
                argv: vec!["vim".to_string(), "main.rs".to_string()],
                ensure: true,
            },
            OutputFormat::Json,
        )
        .await
        .unwrap();
        assert_eq!(created["created"], true);
        handle_kill(&mut client, "dev", Some(7), OutputFormat::Json)
            .await
            .unwrap();
        handle_rename(&mut client, "dev", "renamed", Some(8), OutputFormat::Json)
            .await
            .unwrap();
        handle_session_save(&mut client, "dev", None).await.unwrap();
        handle_version(&mut client, OutputFormat::Json)
            .await
            .unwrap();

        let requests = finish_rpc_script(client, task, requests).await;
        assert_eq!(requests[0]["method"], "session.list");
        assert_eq!(requests[1]["method"], "session.ensure");
        assert_eq!(requests[1]["params"]["name"], "dev");
        assert!(
            requests[1]["params"]["cwd"]
                .as_str()
                .unwrap()
                .ends_with("relative")
        );
        assert_eq!(requests[1]["params"]["pane_title"], "agent");
        assert_eq!(
            requests[1]["params"]["command"],
            serde_json::json!(["vim", "main.rs"])
        );
        assert_eq!(requests[2]["method"], "session.kill");
        assert_eq!(requests[2]["params"]["expected_version"], 7);
        assert_eq!(requests[3]["method"], "session.rename");
        assert_eq!(requests[3]["params"]["new_name"], "renamed");
        assert_eq!(requests[4]["method"], "session.export_template");
        assert_eq!(requests[5]["method"], "system.version");
    }

    /// codex P6 round-1 major 1, test (a): a session whose NAME is a
    /// UUID-shaped string (matching no real id) must remain addressable via
    /// `-s` and killable — the id-first resolution falls back to name lookup
    /// and targets the session's REAL id.
    #[tokio::test]
    async fn uuid_shaped_session_name_falls_back_to_name_lookup() {
        let real_id = "33333333-3333-4333-8333-333333333333";
        let uuid_shaped_name = "00000000-0000-4000-8000-000000000001";
        let list = serde_json::json!({
            "sessions": [{
                "id": real_id,
                "name": uuid_shaped_name,
                "active_window_id": "22222222-2222-4222-8222-222222222222",
                "created_at": 0
            }]
        });

        // Addressable via -s (resolve_session_id path).
        let (mut client, requests, task) = spawn_rpc_script(vec![list.clone()]);
        let resolved = resolve_session_id(&mut client, uuid_shaped_name)
            .await
            .unwrap();
        assert_eq!(resolved, real_id, "name fallback must yield the REAL id");
        let requests = finish_rpc_script(client, task, requests).await;
        assert_eq!(requests[0]["method"], "session.list");
        assert_eq!(requests[0]["params"]["include_scratch"], true);

        // Killable (handle_kill path) — the kill RPC targets the real id.
        let (mut client, requests, task) =
            spawn_rpc_script(vec![list, serde_json::json!({"killed": uuid_shaped_name})]);
        handle_kill(&mut client, uuid_shaped_name, None, OutputFormat::Json)
            .await
            .unwrap();
        let requests = finish_rpc_script(client, task, requests).await;
        assert_eq!(requests[1]["method"], "session.kill");
        assert_eq!(
            requests[1]["params"]["id"], real_id,
            "kill must target the session RESOLVED BY NAME, not the raw arg"
        );
        assert!(requests[1]["params"].get("name").is_none());
    }

    /// claude P6 round-1 extra: `Uuid::parse_str` ALSO accepts the 32-hex
    /// SIMPLE form — a session NAMED e.g. `deadbeef…` (32 hex chars) hits
    /// the same trap. Name fallback must cover it, AND a 32-hex arg that
    /// denotes a REAL session id (in canonical hyphenated form) must
    /// id-match through NORMALIZATION, not raw string equality.
    #[tokio::test]
    async fn simple_form_32hex_input_normalizes_and_falls_back() {
        // (i) session NAMED a 32-hex string, matching no real id → name
        // fallback resolves to its real id.
        let real_id = "33333333-3333-4333-8333-333333333333";
        let hex_name = "deadbeefdeadbeefdeadbeefdeadbeef"; // parses as a UUID
        let list = serde_json::json!({
            "sessions": [{ "id": real_id, "name": hex_name, "created_at": 0 }]
        });
        let (mut client, requests, task) = spawn_rpc_script(vec![list]);
        let resolved = resolve_session_id(&mut client, hex_name).await.unwrap();
        assert_eq!(
            resolved, real_id,
            "32-hex NAME must fall back to name lookup"
        );
        finish_rpc_script(client, task, requests).await;

        // (ii) 32-hex arg denoting a REAL session id → id-match via the
        // normalized hyphenated form; kill targets the canonical id.
        let hyphenated = "11111111-1111-4111-8111-111111111111";
        let simple = "11111111111141118111111111111111";
        let list = serde_json::json!({
            "sessions": [{ "id": hyphenated, "name": "dev", "created_at": 0 }]
        });
        let (mut client, requests, task) =
            spawn_rpc_script(vec![list, serde_json::json!({"killed": "dev"})]);
        handle_kill(&mut client, simple, None, OutputFormat::Json)
            .await
            .unwrap();
        let requests = finish_rpc_script(client, task, requests).await;
        assert_eq!(requests[1]["method"], "session.kill");
        assert_eq!(
            requests[1]["params"]["id"], hyphenated,
            "simple-form input must id-match through normalization and send the canonical id"
        );
    }

    /// codex P6 round-1 major 1, test (b): when the argument matches a REAL
    /// session id, the id wins — even when a DIFFERENT session is NAMED that
    /// same string (documented precedence; a warning is printed).
    #[tokio::test]
    async fn uuid_arg_matching_real_id_wins_over_name_match() {
        let arg = "11111111-1111-4111-8111-111111111111";
        let other_id = "44444444-4444-4444-8444-444444444444";
        let list = serde_json::json!({
            "sessions": [
                { "id": arg, "name": "dev", "created_at": 0 },
                // A different session NAMED the same UUID string — genuine
                // ambiguity; the id must win.
                { "id": other_id, "name": arg, "created_at": 0 }
            ]
        });
        let (mut client, requests, task) =
            spawn_rpc_script(vec![list, serde_json::json!({"killed": "dev"})]);
        handle_kill(&mut client, arg, None, OutputFormat::Json)
            .await
            .unwrap();
        let requests = finish_rpc_script(client, task, requests).await;
        assert_eq!(requests[1]["method"], "session.kill");
        assert_eq!(
            requests[1]["params"]["id"], arg,
            "id match must win over the name match (documented precedence)"
        );
    }

    /// codex P6 round-1 major 1, test (c): a bogus UUID matching neither an
    /// id nor a name is passed through as an id and surfaces the server's
    /// canonical not-found error cleanly.
    #[tokio::test]
    async fn bogus_uuid_neither_id_nor_name_errors_cleanly() {
        let bogus = "99999999-9999-4999-8999-999999999999";
        let list = serde_json::json!({
            "sessions": [{ "id": "11111111-1111-4111-8111-111111111111",
                           "name": "dev", "created_at": 0 }]
        });
        let not_found = serde_json::json!({
            "error": { "code": -32004, "message": "resource not found",
                       "data": { "resource": "session", "id": bogus } }
        });
        let (mut client, requests, task) = spawn_rpc_script(vec![list, not_found]);
        let err = handle_kill(&mut client, bogus, None, OutputFormat::Json)
            .await
            .expect_err("bogus UUID must surface the server's not-found");
        assert!(
            err.to_string().contains("not found"),
            "error must be the canonical not-found, got: {err}"
        );
        let requests = finish_rpc_script(client, task, requests).await;
        assert_eq!(requests[1]["method"], "session.kill");
        assert_eq!(requests[1]["params"]["id"], bogus);
    }

    #[tokio::test]
    async fn cli_window_and_pane_handlers_resolve_names_and_forward_params() {
        let sid = "11111111-1111-4111-8111-111111111111";
        let wid = "22222222-2222-4222-8222-222222222222";
        let pane = "33333333-3333-4333-8333-333333333333";
        let target = "44444444-4444-4444-8444-444444444444";
        let session = || session_list_response(sid, wid);
        let windows = || window_list_response(wid, pane);
        let mut responses = Vec::new();
        responses.extend([
            session(),
            serde_json::json!({"id": "new-window", "title": "editor", "index": 1}),
            session(),
            windows(),
            serde_json::json!({"killed": wid}),
            session(),
            windows(),
            serde_json::json!({"id": wid, "title": "renamed"}),
            session(),
            windows(),
            serde_json::json!({"id": wid, "previous_window_id": null}),
            session(),
            windows(),
            serde_json::json!({"id": wid, "index": 0}),
            session(),
            windows(),
            serde_json::json!([{"id": pane, "cwd": "/tmp", "command": "bash", "is_focused": true, "is_zoomed": false}]),
            windows(),
            session(),
            windows(),
            serde_json::json!({"pane": {"id": target}, "split_from": pane}),
            session(),
            windows(),
            serde_json::json!({"pane_id": pane}),
            session(),
            windows(),
            serde_json::json!({"pane_id": target}),
            session(),
            windows(),
            serde_json::json!({"pane_id": pane}),
            session(),
            windows(),
            serde_json::json!({"pane_id": pane, "is_zoomed": true}),
            session(),
            windows(),
            serde_json::json!({"pane_a": pane, "pane_b": target}),
            session(),
            windows(),
            serde_json::json!({"pane_id": pane, "title": "logs"}),
            session(),
            windows(),
            serde_json::json!({"killed": pane}),
        ]);
        let (mut client, requests, task) = spawn_rpc_script(responses);

        handle_window_new(
            &mut client,
            "dev",
            Some("editor".to_string()),
            Some(std::path::PathBuf::from("/tmp")),
            Some("echo hi".to_string()),
            vec![],
            false,
            OutputFormat::Json,
        )
        .await
        .unwrap();
        handle_window_kill(&mut client, "dev", "main", Some(3), OutputFormat::Json)
            .await
            .unwrap();
        handle_window_rename(
            &mut client,
            "dev",
            "main",
            "renamed",
            None,
            OutputFormat::Json,
        )
        .await
        .unwrap();
        handle_window_focus(&mut client, "dev", "0", None, OutputFormat::Json)
            .await
            .unwrap();
        handle_window_reorder(&mut client, "dev", "main", 0, Some(4), OutputFormat::Json)
            .await
            .unwrap();
        handle_pane_list(&mut client, "dev", Some("main"), OutputFormat::Json)
            .await
            .unwrap();
        handle_pane_split(
            &mut client,
            "dev",
            Some("main"),
            Some(pane),
            Some("horizontal"),
            Some(0.4),
            OutputFormat::Json,
        )
        .await
        .unwrap();
        handle_pane_focus(&mut client, "dev", Some("main"), pane, OutputFormat::Json)
            .await
            .unwrap();
        handle_pane_focus_dir(
            &mut client,
            "dev",
            Some("main"),
            "right",
            OutputFormat::Json,
        )
        .await
        .unwrap();
        handle_pane_resize(
            &mut client,
            "dev",
            Some("main"),
            Some(pane),
            "vertical",
            Some(0.2),
            Some(9),
            OutputFormat::Json,
        )
        .await
        .unwrap();
        handle_pane_zoom(
            &mut client,
            "dev",
            Some("main"),
            Some(pane),
            None,
            OutputFormat::Json,
        )
        .await
        .unwrap();
        handle_pane_swap(
            &mut client,
            "dev",
            Some("main"),
            pane,
            target,
            Some(10),
            OutputFormat::Json,
        )
        .await
        .unwrap();
        handle_pane_title(
            &mut client,
            "dev",
            Some("main"),
            Some(pane),
            Some("logs"),
            false,
            false,
            true,
            OutputFormat::Json,
        )
        .await
        .unwrap();
        handle_pane_kill(
            &mut client,
            "dev",
            Some("main"),
            pane,
            Some(11),
            OutputFormat::Json,
        )
        .await
        .unwrap();

        let requests = finish_rpc_script(client, task, requests).await;
        let methods: Vec<_> = requests
            .iter()
            .map(|r| r["method"].as_str().unwrap())
            .collect();
        assert!(methods.contains(&"window.create"));
        assert!(methods.contains(&"window.kill"));
        assert!(methods.contains(&"pane.split"));
        assert!(methods.contains(&"pane.set_title"));

        let window_create = requests
            .iter()
            .find(|r| r["method"] == "window.create")
            .unwrap();
        assert_eq!(
            window_create["params"]["command"],
            serde_json::json!(["sh", "-c", "echo hi"])
        );
        let pane_split = requests
            .iter()
            .find(|r| r["method"] == "pane.split")
            .unwrap();
        assert_eq!(pane_split["params"]["direction"], "horizontal");
        assert_eq!(pane_split["params"]["ratio"], 0.4);
        let pane_title = requests
            .iter()
            .find(|r| r["method"] == "pane.set_title")
            .unwrap();
        assert_eq!(pane_title["params"]["title"], "logs");
        assert_eq!(pane_title["params"]["auto"], false);
    }

    #[tokio::test]
    async fn cli_pane_io_and_snapshot_handlers_forward_payloads() {
        let sid = "11111111-1111-4111-8111-111111111111";
        let wid = "22222222-2222-4222-8222-222222222222";
        let pane = "33333333-3333-4333-8333-333333333333";
        let session = || session_list_response(sid, wid);
        let windows = || window_list_response(wid, pane);
        let png_b64 = "iVBORw0KGgo=";
        let record_dir = tempfile::tempdir().unwrap();
        let record_path = record_dir.path().join("record.raw");
        let (mut client, requests, task) = spawn_rpc_script(vec![
            session(),
            windows(),
            serde_json::json!({"pane_id": pane, "bytes_written": 5}),
            session(),
            windows(),
            serde_json::json!({"command_id": "cmd-1", "state": "running"}),
            session(),
            windows(),
            serde_json::json!({"pane_id": pane, "text": "ready\n", "lines": 1}),
            serde_json::json!({"pane_id": pane, "matched": true, "elapsed_ms": 12, "absent": false}),
            session(),
            windows(),
            serde_json::json!({"pane_id": pane, "cols": 100, "rows": 30}),
            session(),
            windows(),
            serde_json::json!([{"id": pane, "cwd": "/tmp", "command": "bash", "is_focused": true, "is_zoomed": false}]),
            serde_json::json!({"recording_id": "55555555-5555-4555-8555-555555555555", "pane_id": pane, "path": record_path.display().to_string(), "duration_ms": 0, "lossless": true, "backpressure": true}),
            serde_json::json!({"recording_id": "55555555-5555-4555-8555-555555555555", "path": record_path.display().to_string(), "bytes_written": 0, "status": "complete", "lossless": true, "error": null}),
            session(),
            windows(),
            serde_json::json!({"png_base64": png_b64, "width": 10, "height": 10}),
            session(),
            serde_json::json!({"png_base64": png_b64, "width": 20, "height": 10}),
            session(),
            windows(),
            serde_json::json!({"png_base64": png_b64, "width": 30, "height": 10}),
        ]);

        handle_pane_send_keys(
            &mut client,
            "dev",
            Some("main"),
            Some(pane),
            Some("hello"),
            None,
            OutputFormat::Json,
        )
        .await
        .unwrap();
        handle_pane_run(
            &mut client,
            "dev",
            Some("main"),
            Some(pane),
            "make test",
            60,
            true,
            OutputFormat::Json,
        )
        .await
        .unwrap();
        handle_pane_capture(
            &mut client,
            "dev",
            Some("main"),
            Some(pane),
            5,
            OutputFormat::Json,
        )
        .await
        .unwrap();
        handle_wait_for(
            &mut client,
            Some("dev"),
            None,
            Some(pane),
            Some("ready"),
            None,
            false,
            20,
            1000,
            25,
            OutputFormat::Json,
        )
        .await
        .unwrap();
        handle_pane_set_size(
            &mut client,
            "dev",
            Some("main"),
            Some(pane),
            100,
            30,
            OutputFormat::Json,
        )
        .await
        .unwrap();
        handle_pane_record(
            &mut client,
            "dev",
            pane,
            &record_path,
            true,
            Some(0),
            OutputFormat::Json,
        )
        .await
        .unwrap();
        handle_pane_snapshot(
            &mut client,
            "dev",
            Some("main"),
            Some(pane),
            None,
            OutputFormat::Json,
        )
        .await
        .unwrap();
        handle_snapshot(
            &mut client,
            Some("dev"),
            None,
            None,
            120,
            36,
            OutputFormat::Json,
        )
        .await
        .unwrap();
        handle_snapshot(
            &mut client,
            Some("dev"),
            Some("main"),
            None,
            80,
            24,
            OutputFormat::Json,
        )
        .await
        .unwrap();

        let requests = finish_rpc_script(client, task, requests).await;
        assert!(
            requests
                .iter()
                .any(|r| r["method"] == "pane.send_keys" && r["params"]["text"] == "hello")
        );
        assert!(
            requests
                .iter()
                .any(|r| r["method"] == "pane.run_command" && r["params"]["async"] == true)
        );
        assert!(
            requests
                .iter()
                .any(|r| r["method"] == "pane.wait_for" && r["params"]["poll_ms"] == 25)
        );
        assert!(
            requests
                .iter()
                .any(|r| r["method"] == "pane.set_size" && r["params"]["cols"] == 100)
        );
        assert!(requests.iter().any(|r| {
            r["method"] == "pane.record.start"
                && r["params"]["pane_id"] == pane
                && r["params"]["path"] == record_path.display().to_string()
        }));
        assert!(requests.iter().any(|r| {
            r["method"] == "pane.list"
                && r["params"]["session_id"] == sid
                && r["params"]["window_id"] == wid
        }));
        assert!(requests.iter().any(|r| r["method"] == "pane.snapshot"));
        assert!(requests.iter().any(|r| r["method"] == "session.snapshot"));
        assert!(requests.iter().any(|r| r["method"] == "window.snapshot"));
    }

    #[tokio::test]
    async fn cli_events_plugin_and_apply_handlers_cover_streaming_and_permissions() {
        let event = serde_json::json!({"seq": 42, "type": "plugin.demo.tick", "data": {}});
        let (mut client, requests, task) = spawn_rpc_script(vec![
            serde_json::json!({"events": [event.clone()], "next_seq": 43, "gap": 0, "lagged": false}),
            serde_json::json!({"events": [event], "current_seq": 44}),
            serde_json::json!({"name": "demo", "version": "1.0.0", "pid": 123, "watching": true, "subscribes": ["pane."]}),
            serde_json::json!({"plugins": [{"name": "demo", "version": "1.0.0", "status": "running", "pid": 123, "uptime_ms": 2500}]}),
            serde_json::json!({"name": "demo", "pid": 124}),
            serde_json::json!({"killed": "demo"}),
            serde_json::json!({"granted": true}),
            serde_json::json!({"revoked": true}),
            serde_json::json!({"grants": {"pane.capture": "*"}, "subscribes": {"allowed": ["pane."]}}),
            serde_json::json!({"path": "/tmp/audit.jsonl", "entries": [{"ts": "now", "method": "pane.capture", "decision": "allow", "reason": "grant"}]}),
            serde_json::json!({"correlation_id": "apply-1", "outputs": [{"session_id": "sid"}], "last_event_seq": 50, "spawn_results": [{"pane_id": "pane-1", "spawned": true}]}),
        ]);

        handle_events_watch(
            &mut client,
            vec!["plugin.demo.".to_string()],
            Some(42),
            100,
            Some(1),
        )
        .await
        .unwrap();
        handle_events_history(&mut client, vec!["plugin.demo.".to_string()], 5)
            .await
            .unwrap();
        handle_plugin_install(
            &mut client,
            std::path::Path::new("/tmp/plugin"),
            &["--flag".to_string()],
            Some(std::path::Path::new("/tmp")),
            true,
            OutputFormat::Text,
        )
        .await
        .unwrap();
        handle_plugin_list(&mut client, OutputFormat::Plain)
            .await
            .unwrap();
        handle_plugin_reload(&mut client, "demo", OutputFormat::Text)
            .await
            .unwrap();
        handle_plugin_kill(&mut client, "demo", OutputFormat::Plain)
            .await
            .unwrap();
        handle_plugin_grant(
            &mut client,
            "demo",
            "pane.capture",
            Some("*"),
            false,
            OutputFormat::Plain,
        )
        .await
        .unwrap();
        handle_plugin_revoke(
            &mut client,
            "demo",
            "pane.capture",
            Some("*"),
            true,
            OutputFormat::Text,
        )
        .await
        .unwrap();
        handle_plugin_grants(&mut client, "demo", OutputFormat::Text)
            .await
            .unwrap();
        handle_plugin_audit(&mut client, "demo", 10, OutputFormat::Text)
            .await
            .unwrap();
        handle_apply(
            &mut client,
            vec![shux_core::apply::Op::CreateSession {
                name: Some("dev".to_string()),
                cwd: std::path::PathBuf::from("/tmp"),
                initial_command: Vec::new(),
                initial_window_title: None,
            }],
            false,
            std::path::Path::new("/tmp/shux.sock"),
        )
        .await
        .unwrap();

        let requests = finish_rpc_script(client, task, requests).await;
        let methods: Vec<_> = requests
            .iter()
            .map(|r| r["method"].as_str().unwrap())
            .collect();
        for method in [
            "events.watch",
            "events.history",
            "plugin.install",
            "plugin.list",
            "plugin.reload",
            "plugin.kill",
            "plugin.grant",
            "plugin.revoke",
            "plugin.grants",
            "plugin.audit",
            "state.apply",
        ] {
            assert!(methods.contains(&method), "missing RPC call {method}");
        }
        let grant = requests
            .iter()
            .find(|r| r["method"] == "plugin.grant")
            .unwrap();
        assert_eq!(grant["params"]["plugin"], "demo");
        assert_eq!(grant["params"]["method"], "pane.capture");
        let apply = requests
            .iter()
            .find(|r| r["method"] == "state.apply")
            .unwrap();
        assert_eq!(apply["params"]["ops"][0]["op"], "create_session");
    }

    #[tokio::test]
    async fn rpc_call_surfaces_structured_errors_and_frame_limits() {
        let (mut client, requests, task) = spawn_rpc_script(vec![serde_json::json!({
            "error": {
                "code": -32002,
                "message": "version_conflict",
                "data": {
                    "resource": "pane",
                    "id": "p1",
                    "expected_version": 1,
                    "actual_version": 2
                }
            }
        })]);

        let err = rpc_call(&mut client, "pane.kill", serde_json::json!({}))
            .await
            .unwrap_err();
        let rendered = err.to_string();
        assert!(rendered.contains("version_conflict"));
        assert!(rendered.contains("expected 1, actual 2"));
        let requests = finish_rpc_script(client, task, requests).await;
        assert_eq!(requests[0]["method"], "pane.kill");

        let (mut client, mut server) = tokio::net::UnixStream::pair().unwrap();
        let oversized = tokio::spawn(async move {
            let mut len_buf = [0u8; 4];
            server.read_exact(&mut len_buf).await.unwrap();
            let len = u32::from_be_bytes(len_buf) as usize;
            let mut payload = vec![0u8; len];
            server.read_exact(&mut payload).await.unwrap();
            let too_large = 16 * 1024 * 1024 + 1;
            server
                .write_all(&(too_large as u32).to_be_bytes())
                .await
                .unwrap();
        });
        let err = rpc_call(&mut client, "system.version", serde_json::json!({}))
            .await
            .unwrap_err();
        assert!(matches!(err, RpcClientError::FrameTooLarge(_)));
        oversized.await.unwrap();
    }
}
