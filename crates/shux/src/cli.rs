//! CLI argument definitions and subcommand dispatch.
//!
//! Every `shux` subcommand is a thin wrapper over a JSON-RPC call to the daemon
//! (PRD §4.3 invariant 2: "CLI == API").

use std::path::PathBuf;

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
        "  {} --params '{{\"name\":\"demo\",\"command\":[\"lazygit\"]}}'\n",
        shux("rpc call session.create"),
    ));
    s.push_str(&format!(
        "  {dim}# Or the noun-verb form (identical effect):{r}\n"
    ));
    s.push_str(&format!("  {} demo -- lazygit\n\n", shux("session create"),));
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
        &format!("{} {dim}(sealed data plane){r}", m("pane.output.watch")),
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

        /// Shell command to run in the initial pane (single string).
        #[arg(long)]
        cmd: Option<String>,

        /// Trailing argv after `--` — exec'd directly (no shell wrapper).
        #[arg(last = true, num_args = 0..)]
        argv: Vec<String>,
    },

    /// List sessions.
    #[command(alias = "ls")]
    List,

    /// Kill a session.
    Kill {
        /// Session name (positional or `-s/--session`).
        #[arg(value_name = "NAME")]
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
    /// chunk to stdout. Pipes cleanly:
    /// `shux pane watch -p X | tee log`. Output is rate-limited at
    /// the source to ~10 chunks/sec/pane to prevent noisy panes from
    /// drowning subscribers. Bytes that arrived before the first
    /// poll are UNREACHABLE — the data plane is intentionally lossy
    /// to keep terminal secrets out of any history.
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
    format: OutputFormat,
) -> anyhow::Result<()> {
    let result = rpc_call(
        stream,
        "session.list",
        serde_json::Value::Object(Default::default()),
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
                            style::SessionInfo {
                                name,
                                id,
                                window_count,
                                created,
                                is_active: false, // no attach tracking yet
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
# bundled NF JetBrains Mono and Noto Emoji stay in the fallback chain
# so any glyph your font lacks (typical for plain non-patched
# typefaces, or any standalone emoji) still resolves — no tofu either
# way. Doesn't affect live attach (your terminal font controls that).
# Font changes hot-reload: edit this line and the next snapshot uses
# the new font. On a bad path the last-good rasterizer is retained
# and a warning is logged.
# font = "/path/to/your-font.ttf"
#
# Caveat for status-bar segments: stick to ASCII + Nerd Font
# private-use codepoints in script segment output. Obscure BMP
# glyphs like ⎈ (U+2388, kubectl helm) and ⎇ (U+2387, alt-branch)
# are NOT in JetBrains Mono or any Nerd Font — they'll tofu in
# PNG snapshots. Use NF equivalents instead. Paste either the
# literal glyph (UTF-8) into a single-quoted TOML string, or use
# TOML's escape inside a double-quoted string. Note TOML uses
# bare \uXXXX (4-hex BMP) or \UXXXXXXXX (8-hex, supplementary
# plane) — NOT Rust's \u{XXXX} form:
#   nf-pl-branch      U+E0A0   ''  or  "\uE0A0"
#   nf-md-kubernetes  U+F10FE  '󱃾'  or  "\U000F10FE"
#   nf-md-ship_wheel  U+F124A  '󱉊'  or  "\U000F124A"
# Standalone monochrome emoji (🍺 🧩 🦀 🚀 ⚡ …) render correctly in
# PNG snapshots via the bundled Noto Emoji fallback — no extra
# configuration needed. Colour emoji and composed emoji (ZWJ
# sequences like 👨‍💻, VS16 like 🛠️, regional-indicator flag pairs,
# skin-tone modifiers) are not yet supported — the parser splits
# them into separate cells. For composed glyphs in status bars,
# configure your starship language modules with the NF equivalent.
# Example for rust: symbol = ' ' (or
# symbol = "\uE7A8 " using TOML escape syntax).

[keys]
# Prefix key (e.g. "ctrl-space", "ctrl-b", "alt-w")
prefix = "ctrl-space"

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
# the spawned `starship prompt` invocation. Your shell PS1 (driven by
# `~/.config/starship.toml`) is unaffected — only the segment spawn
# sees this override.
# ─────────────────────────────────────────────────────────────────────

[[statusbar.segment]]
zone = "right"
command = ["starship", "prompt"]
interval_ms = 1000
fallback = " (starship not installed) "
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
pub async fn handle_new(
    stream: &mut tokio::net::UnixStream,
    session_name: Option<String>,
    cwd: Option<std::path::PathBuf>,
    cmd: Option<String>,
    argv: Vec<String>,
    ensure: bool,
    format: OutputFormat,
) -> anyhow::Result<serde_json::Value> {
    let cwd = cwd
        .map(Ok)
        .unwrap_or_else(std::env::current_dir)
        .map_err(|e| anyhow::anyhow!("failed to determine current directory: {e}"))?;
    let params = build_session_create_params(session_name, cwd, cmd, argv);

    let method = if ensure {
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
            style::print_session_created(name, id, ensure);
        }
    }

    Ok(result)
}

fn build_session_create_params(
    session_name: Option<String>,
    cwd: std::path::PathBuf,
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
pub async fn handle_kill(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    expected_version: Option<u64>,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let mut params = serde_json::Map::new();
    params.insert(
        "name".to_string(),
        serde_json::Value::String(session_name.to_string()),
    );
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

/// Resolve a session name to its UUID by querying session.list.
async fn resolve_session_id(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
) -> Result<String, RpcClientError> {
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
            // Get active window from session
            let result = rpc_call(stream, "session.list", serde_json::json!({})).await?;
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
                match format {
                    OutputFormat::Json => {
                        let _ = writeln!(out, "{}", serde_json::to_string(chunk)?);
                    }
                    OutputFormat::Text | OutputFormat::Plain => {
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

pub async fn handle_plugin_install(
    stream: &mut tokio::net::UnixStream,
    path: &std::path::Path,
    args: &[String],
    cwd: Option<&std::path::Path>,
    watch: bool,
    format: OutputFormat,
) -> anyhow::Result<()> {
    use crate::style;

    let mut params = serde_json::Map::new();
    params.insert(
        "path".into(),
        serde_json::Value::String(path.display().to_string()),
    );
    if !args.is_empty() {
        params.insert("args".into(), serde_json::json!(args));
    }
    if let Some(cwd) = cwd {
        params.insert(
            "cwd".into(),
            serde_json::Value::String(cwd.display().to_string()),
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
                command: SessionCommand::List
            })
        ));
    }

    #[test]
    fn test_cli_parse_session_list_alias_ls() {
        let cli = Cli::try_parse_from(["shux", "session", "ls"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Session {
                command: SessionCommand::List
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
                        cmd,
                        argv,
                    },
            }) => {
                assert!(name.is_none());
                assert_eq!(session, Some("work".to_string()));
                assert!(ensure);
                assert!(detached);
                assert!(cwd.is_none());
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
    fn test_build_session_create_params_always_includes_cwd() {
        let params = build_session_create_params(
            Some("demo".to_string()),
            std::path::PathBuf::from("/tmp/shux-demo"),
            None,
            vec!["pwd".to_string()],
        );

        assert_eq!(params.get("name").and_then(|v| v.as_str()), Some("demo"));
        assert_eq!(
            params.get("cwd").and_then(|v| v.as_str()),
            Some("/tmp/shux-demo")
        );
        assert_eq!(params.get("command"), Some(&serde_json::json!(["pwd"])));
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

    /// Session aliases `ses` and `sess` parse identically.
    #[test]
    fn test_cli_parse_session_alias() {
        let cli = Cli::try_parse_from(["shux", "ses", "list"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Session {
                command: SessionCommand::List
            })
        ));
        let cli = Cli::try_parse_from(["shux", "sess", "list"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Session {
                command: SessionCommand::List
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
}
