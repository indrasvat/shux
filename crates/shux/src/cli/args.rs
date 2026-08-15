//! The clap definitions: every subcommand, flag and value parser.
//!
//! Nothing here talks to the daemon. Parsing is separated from doing so that
//! a change to an argument's shape cannot quietly change what it does.

use super::help::*;

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

use crate::features::plugin::PluginScaffoldRuntime;

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

    /// The background daemon that owns every session, pane, and PTY.
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },

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

        /// Check the template and print the lowered ops without applying.
        /// Catches everything decidable without the daemon (shape, argv,
        /// window titles); conflicts with live state — a session name already
        /// in use — can only surface on the real apply.
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

        /// Shell command for the initial pane, run by the daemon's shell
        /// (its `$SHELL -c`, falling back to `/bin/sh` — the daemon outlives
        /// your terminal, so its environment is the one that applies) — so
        /// pipes, `;`, `&&`, quoting, globs and redirection all work:
        /// `--cmd "cargo watch -x test 2>&1 | tee build.log"`.
        /// Omitted or empty opens your login+interactive shell.
        /// For exec-style passthrough with no shell at all, use trailing
        /// `--` instead.
        ///
        /// A command that itself starts with a dash (`--cmd "-n is a valid
        /// sed script"`) is taken as the command, not as a flag.
        #[arg(long, value_name = "SHELL_COMMAND", allow_hyphen_values = true)]
        cmd: Option<String>,

        /// Trailing argv after `--` — exec'd directly (no shell wrapper, no
        /// splitting, no expansion). Takes precedence over `--cmd`.
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
        /// Session name OR id (positional or `-s/--session`; issue #88 —
        /// a UUID (e.g. the `session_id` a `lens run` response returns for
        /// a hidden scratch session) works here too, not just names, and
        /// issue #120 — so does the 8-character short id `session list`
        /// prints, or any unambiguous prefix.
        /// Precedence: an exact NAME or a full UUID beats an id prefix.
        /// Between the two exact forms the ID wins, with a warning.
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
        /// Session name or id (full UUID, or the short form `session list` prints)
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

/// Daemon lifecycle. The daemon auto-starts on first use; this is how you stop it.
#[derive(Subcommand, Debug)]
pub enum DaemonCommand {
    /// Stop the daemon for THIS runtime dir, gracefully (SIGTERM).
    ///
    /// Every shux invocation starts a daemon if none is running, and it outlives the
    /// command — so a scripted or CI run leaks one unless it is stopped. This reaps
    /// exactly the daemon recorded in `$XDG_RUNTIME_DIR/shux/shux.pid`, never other
    /// checkouts' or other agents' daemons the way a `pkill -f shux` would. Exits 0 when
    /// no daemon is running, so it is safe in a cleanup trap.
    Stop,

    /// Report whether a daemon is running for this runtime dir, and its pid.
    Status,
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
        /// Session name or id (full UUID, or the short form `session list` prints)
        #[arg(short, long)]
        session: String,
    },

    /// Create a new window in a session. Mirrors `window.create` RPC.
    Create {
        /// Session name or id (full UUID, or the short form `session list` prints)
        #[arg(short, long)]
        session: String,

        /// Window name (auto-generated if not provided)
        #[arg(short, long)]
        name: Option<String>,

        /// Working directory for the new window's initial pane.
        /// Defaults to the daemon's current working directory.
        #[arg(long)]
        cwd: Option<std::path::PathBuf>,

        /// Shell command for the new window's initial pane, run by the
        /// daemon's shell (its `$SHELL -c`, falling back to `/bin/sh`) —
        /// pipes, `;`, `&&`, quoting, globs and redirection all work.
        /// Empty / omitted spawns the user's login+interactive shell.
        /// For exec-style passthrough use trailing `--` instead:
        /// `shux window create -s X -n W -- vim foo.rs`.
        ///
        /// A command starting with a dash is taken as the command, not a flag.
        #[arg(long, value_name = "SHELL_COMMAND", allow_hyphen_values = true)]
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
        /// Session name or id (full UUID, or the short form `session list` prints)
        #[arg(short, long)]
        session: String,

        /// Window name, index, or id
        #[arg(short, long)]
        window: String,

        /// Optimistic concurrency: only succeed if the window is at
        /// this version. See `shux session kill --help` for details.
        #[arg(long)]
        expected_version: Option<u64>,
    },

    /// Rename a window
    Rename {
        /// Session name or id (full UUID, or the short form `session list` prints)
        #[arg(short, long)]
        session: String,

        /// Window name, index, or id (full UUID or short form)
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
        /// Session name or id (full UUID, or the short form `session list` prints)
        #[arg(short, long)]
        session: String,

        /// Window name, index, or id
        #[arg(short, long)]
        window: String,

        /// Optimistic concurrency: only succeed if the window is at
        /// this version.
        #[arg(long)]
        expected_version: Option<u64>,
    },

    /// Reorder (move) a window to a new index
    Reorder {
        /// Session name or id (full UUID, or the short form `session list` prints)
        #[arg(short, long)]
        session: String,

        /// Window name, index, or id
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
        /// Window index, name, or id (full UUID or the short form
        /// `window list` prints). If omitted, the session's
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
pub fn parse_duration_ms(s: &str) -> Result<u64, String> {
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

/// Parse a split ratio, enforcing the range the flag has always documented.
///
/// The layout engine clamps an out-of-range ratio instead of refusing it, so
/// `--ratio 5.0` used to exit 0 and hand back a ~3-column sliver too narrow to
/// draw its own border title — a pane that exists but is unusable and
/// unlabelled (issue #136). The range is open at both ends: `0.0` and `1.0`
/// each ask for a zero-width pane.
///
/// A parse error surfaces as a clap usage error → CLI exit 2, matching how an
/// out-of-range direction is already refused.
pub fn parse_ratio(s: &str) -> Result<f64, String> {
    let v: f64 = s
        .trim()
        .parse()
        .map_err(|_| format!("invalid ratio {s:?} (expected a number above 0.0 and below 1.0)"))?;
    // One predicate for every entry point — CLI flag, template and RPC — and
    // it judges the `f32` the daemon will actually store, so a value that
    // only collapses on the cast cannot pass here and fail there.
    crate::pane_command::check_ratio(v).map_err(|e| format!("ratio {e}"))?;
    Ok(v)
}

/// Parse a `pane glance --mask ROW,COL,WIDTH` redaction rect (task 080). All three are
/// `u16`; `WIDTH == 0` is rejected (a zero-width mask redacts nothing — likely a typo).
pub fn parse_mask_rect(s: &str) -> Result<(u16, u16, u16), String> {
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
        /// Session name or id (full UUID, or the short form `session list` prints)
        #[arg(short, long)]
        session: String,

        /// Window name, index, or id (uses active window if not provided)
        #[arg(short, long)]
        window: Option<String>,
    },

    /// Split a pane
    Split {
        /// Session name or id (full UUID, or the short form `session list` prints)
        #[arg(short, long)]
        session: String,

        /// Window name, index, or id (uses active window if not provided)
        #[arg(short, long)]
        window: Option<String>,

        /// Pane id — full UUID or the short form `pane list` prints
        /// (uses active pane if not provided)
        #[arg(short, long)]
        pane: Option<String>,

        /// Split direction: vertical, horizontal, or auto
        #[arg(short, long)]
        direction: Option<String>,

        /// Split ratio — above 0.0 and below 1.0 (default 0.5)
        #[arg(short, long, value_parser = parse_ratio)]
        ratio: Option<f64>,

        /// Shell command for the new pane, run by the daemon's shell (its
        /// `$SHELL -c`, falling back to `/bin/sh`) — pipes, `;`, `&&`,
        /// quoting, globs and redirection all work. Omitted opens the default
        /// login+interactive shell.
        ///
        /// A command starting with a dash is taken as the command, not a flag.
        #[arg(long, value_name = "SHELL_COMMAND", allow_hyphen_values = true)]
        cmd: Option<String>,

        /// Trailing argv after `--` — exec'd directly (no shell wrapper, no
        /// splitting, no expansion). Takes precedence over `--cmd`.
        #[arg(last = true, num_args = 0..)]
        argv: Vec<String>,
    },

    /// Focus a specific pane by id (full UUID or short form)
    Focus {
        /// Session name or id (full UUID, or the short form `session list` prints)
        #[arg(short, long)]
        session: String,

        /// Window name, index, or id (uses active window if not provided)
        #[arg(short, long)]
        window: Option<String>,

        /// Pane id — full UUID or the short form `pane list` prints
        #[arg(short, long)]
        pane: String,
    },

    /// Move focus in a direction (up/down/left/right)
    FocusDir {
        /// Session name or id (full UUID, or the short form `session list` prints)
        #[arg(short, long)]
        session: String,

        /// Window name, index, or id (uses active window if not provided)
        #[arg(short, long)]
        window: Option<String>,

        /// Direction: up, down, left, right
        #[arg(short, long)]
        direction: String,
    },

    /// Resize a pane
    Resize {
        /// Session name or id (full UUID, or the short form `session list` prints)
        #[arg(short, long)]
        session: String,

        /// Window name, index, or id (uses active window if not provided)
        #[arg(short, long)]
        window: Option<String>,

        /// Pane id — full UUID or the short form `pane list` prints
        /// (uses active pane if not provided)
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
        /// Session name or id (full UUID, or the short form `session list` prints)
        #[arg(short, long)]
        session: String,

        /// Window name, index, or id (uses active window if not provided)
        #[arg(short, long)]
        window: Option<String>,

        /// Pane id — full UUID or the short form `pane list` prints
        /// (uses active pane if not provided)
        #[arg(short, long)]
        pane: Option<String>,

        /// Optimistic concurrency: only succeed if the pane is at
        /// this version.
        #[arg(long)]
        expected_version: Option<u64>,
    },

    /// Swap two panes
    Swap {
        /// Session name or id (full UUID, or the short form `session list` prints)
        #[arg(short, long)]
        session: String,

        /// Window name, index, or id (uses active window if not provided)
        #[arg(short, long)]
        window: Option<String>,

        /// First pane id — full UUID or short form
        #[arg(short, long)]
        pane: String,

        /// Second pane id (target to swap with) — full UUID or short form
        #[arg(short, long)]
        target: String,

        /// Optimistic concurrency: only succeed if pane (first) is at
        /// this version.
        #[arg(long)]
        expected_version: Option<u64>,
    },

    /// Kill a pane
    Kill {
        /// Session name or id (full UUID, or the short form `session list` prints)
        #[arg(short, long)]
        session: String,

        /// Window name, index, or id (uses active window if not provided)
        #[arg(short, long)]
        window: Option<String>,

        /// Pane id to kill — full UUID or the short form `pane list` prints
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
        /// Session name or id (full UUID, or the short form `session list` prints)
        #[arg(short, long)]
        session: String,

        /// Window name, index, or id (uses active window if not provided)
        #[arg(short, long)]
        window: Option<String>,

        /// Pane id — full UUID or the short form `pane list` prints
        /// (uses active pane if not provided)
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

        /// Pane id to watch — full UUID or the short form `pane list` prints.
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

        /// Pane id to record — full UUID or the short form `pane list` prints.
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
        /// Session name or id (full UUID, or the short form `session list` prints)
        #[arg(short, long)]
        session: String,

        /// Window name, index, or id (uses active window if not provided)
        #[arg(short, long)]
        window: Option<String>,

        /// Pane id — full UUID or the short form `pane list` prints
        /// (uses active pane if not provided)
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
        /// Session name or id (full UUID, or the short form `session list` prints)
        #[arg(short, long)]
        session: String,

        /// Window name, index, or id (uses active window if not provided)
        #[arg(short, long)]
        window: Option<String>,

        /// Pane id — full UUID or the short form `pane list` prints
        /// (uses active pane if not provided)
        #[arg(short, long)]
        pane: Option<String>,

        /// Shell command to run, typed into the pane's live shell.
        ///
        /// This is shell TEXT, not an argv: it is written to the pane's
        /// standard input exactly as given, so `;`, `|`, `&&`, `$(…)`,
        /// redirection and globs are all active — the same contract as
        /// `session create --cmd`. Quote anything you mean literally.
        ///
        /// The pane must have something reading its input. A pane whose
        /// program replaced the shell (`--cmd '… exec top'`) will show the
        /// line on screen and never run it.
        ///
        /// The `pane.run_command` RPC additionally takes an `args` array,
        /// whose elements ARE arguments — shux quotes each one, so a value
        /// containing spaces or metacharacters stays a single argument. There
        /// is no CLI flag for it; use `shux rpc call` when you need it.
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
        /// Session name or id (full UUID, or the short form `session list` prints)
        #[arg(short, long)]
        session: String,

        /// Window name, index, or id (uses active window if not provided)
        #[arg(short, long)]
        window: Option<String>,

        /// Pane id — full UUID or the short form `pane list` prints
        /// (uses active pane if not provided)
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
        /// Session name or id (full UUID, or the short form `session list` prints)
        #[arg(short, long)]
        session: String,
        /// Window name, index, or id (uses active window if not provided)
        #[arg(short, long)]
        window: Option<String>,
        /// Pane id — full UUID or the short form `pane list` prints
        /// (uses active pane if not provided)
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
        /// Pane id — full UUID, or any unambiguous prefix of one such as
        /// the 8-character short form `pane list` prints.
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
        /// Pane id — full UUID or any unambiguous prefix, such as the
        /// 8-character short form `pane list` prints (mirrors the RPC
        /// `pane_id`).
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

        /// Frame-content HOLD (task 083): settle once the presented frame has
        /// stayed UNCHANGED for this long, even while output keeps pumping
        /// (silence counts as held). This is the RECOMMENDED settle for an
        /// animated TUI — it handles both a continuous repainter AND a TUI that
        /// reaches a static steady state (stops repainting), and rejects a slow
        /// spinner that quiet-mode false-settles between frames. `0` (default)
        /// = off. Human duration (`600ms`); range [10ms, 60s] and ≤ --timeout
        /// — out of range → exit 2.
        #[arg(long = "hold-ms", default_value = "0", value_parser = parse_duration_ms)]
        hold_ms: u64,

        /// Frame-content STABLE-FRAMES (task 083): settle once this many
        /// CONTIGUOUS revisions render an identical frame — a count-based
        /// alternative to --hold-ms for a TUI that repaints CONTINUOUSLY.
        /// NOTE: a pane that reaches a STATIC steady state (STOPS repainting)
        /// never produces K new revisions, so it times out as
        /// `settle_never_stable`; for such a TUI use --hold-ms (or default
        /// quiet), which count silence as held. `1` (default) = off; range
        /// [1, 1000]. Never reaching K within --timeout is a FAILURE
        /// (`settle_never_stable`), never infra.
        #[arg(long = "stable-frames", default_value_t = 1)]
        stable_frames: u32,
    },

    /// Capture the pane's current visible frame as a checkpoint for a later
    /// `pane diff`. Mirrors `pane.checkpoint` RPC (lens PRD §7). At most 4
    /// checkpoints per pane; a 5th evicts the oldest by creation revision
    /// (FIFO). Re-checkpointing the same revision is a no-op. Prints the
    /// keyed revision and any evicted revision.
    Checkpoint {
        /// Pane id — full UUID or any unambiguous prefix, such as the
        /// 8-character short form `pane list` prints (mirrors the RPC
        /// `pane_id`).
        #[arg(value_name = "PANE")]
        pane: String,
    },

    /// Diff the pane's current visible frame against a checkpointed revision.
    /// Mirrors `pane.diff_since` RPC (lens PRD §7). Prints the structured
    /// delta (changed cell count, per-row spans, changed row text). Exit 0 on
    /// any delta (diff is data, not a verdict); exit 5 on STALE_REVISION /
    /// RESIZE_INVALIDATED / PAYLOAD_TOO_LARGE (oversized heat PNG).
    Diff {
        /// Pane id — full UUID or any unambiguous prefix, such as the
        /// 8-character short form `pane list` prints (mirrors the RPC
        /// `pane_id`).
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
        /// Session name or id (full UUID, or the short form `session list` prints)
        #[arg(short, long)]
        session: String,
        /// Window name, index, or id (uses active window if not provided)
        #[arg(short, long)]
        window: Option<String>,
        /// Pane id — full UUID or the short form `pane list` prints
        /// (uses active pane if not provided)
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
        /// Window index, name, or id (full UUID or short form).
        #[arg(short, long)]
        window: Option<String>,
        /// Explicit pane id — full UUID or short form. REQUIRED for multi-pane workspaces
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
pub const LENS_LOOP_RECIPE: &str = "\
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
    /// The scenario file (`name`, `command`, optional `cwd` relative to the scenario dir,
    /// `[terminal]`, `[env]`, `[[steps]]`)
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
    /// 3 infra · 4 could not write the report · 5 child died · 6 update refused). A frame
    /// with no committed golden is a CI-safe regression (`missing_golden`) unless
    /// `--on-missing create`. `--update`
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
        tol: Option<crate::gate::cell_compare::TolParams>,

        /// Directory for scratch evidence (heat PNGs). Default `.shux/out/<scenario>/`.
        #[arg(long, value_name = "DIR")]
        out: Option<PathBuf>,

        /// Retry budget for a flaky frame (task 083): on a compare MISMATCH, re-settle +
        /// re-capture up to N times before failing. A retry redeems FAIL→PASS only by matching
        /// the golden (never by consensus among failing captures); a per-step `expect_golden.
        /// retries` can raise this floor further for one frame.
        #[arg(long, value_name = "N")]
        retries: Option<u32>,

        /// Attach a replayable asciinema v2 `.cast` of the whole run beside the report (task
        /// 083) so a reviewer can scrub how the TUI reached a failing frame. Optional PATH
        /// (default `<out>/<scenario>.cast`). EPHEMERAL — written under the gitignored out dir,
        /// never a golden. Armed at spawn (captures startup + resizes).
        #[arg(long, value_name = "PATH", num_args = 0..=1, default_missing_value = "")]
        cast: Option<String>,

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
pub fn parse_tol(s: &str) -> Result<crate::gate::cell_compare::TolParams, String> {
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
    Ok(crate::gate::cell_compare::TolParams {
        max_channel_delta,
        max_changed_frac,
    })
}

/// Parse a `COLSxROWS` size flag (e.g. `80x24`) into `(cols, rows)`. Shape
/// only — range bounds are an RPC-level INVALID_PARAMS, not a clap usage
/// error (matches the settle `--quiet`/`--timeout` convention: the CLI
/// normalizes shape, the server owns the range contract).
pub fn parse_size_cxr(s: &str) -> Result<(u16, u16), String> {
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
pub fn parse_env_kv(s: &str) -> Result<(String, String), String> {
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
}
