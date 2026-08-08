//! Entity types for the shux data model (PRD 5.1).
//!
//! Defines Session, Window, Pane and their ID types.
//! All entities carry version stamps for optimistic concurrency.

use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A newtype wrapper around UUID for type-safe entity identification.
macro_rules! define_id {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, Hash, Eq, PartialEq, Serialize, Deserialize)]
        pub struct $name(pub Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            pub fn from_uuid(uuid: Uuid) -> Self {
                Self(uuid)
            }

            pub fn as_uuid(&self) -> &Uuid {
                &self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl From<Uuid> for $name {
            fn from(uuid: Uuid) -> Self {
                Self(uuid)
            }
        }

        impl std::str::FromStr for $name {
            type Err = uuid::Error;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Ok(Self(Uuid::parse_str(s)?))
            }
        }
    };
}

define_id!(SessionId);
define_id!(WindowId);
define_id!(PaneId);
define_id!(PluginId);

/// Restart policy for a pane's child process (PRD 5.1, 6.2).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RestartPolicy {
    #[default]
    Never,
    OnFail,
    Always,
}

/// A reference to a named theme (PRD 5.3 cascade).
pub type ThemeRef = String;

/// Tags are arbitrary key-value metadata visible to plugins (PRD 5.1).
pub type Tags = HashMap<String, String>;

/// Monotonically increasing version stamp for optimistic concurrency (PRD 5.4).
pub type Version = u64;

/// A session groups windows and represents a named workspace (PRD 5.1).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: SessionId,
    pub name: String,
    pub created_at: SystemTime,
    /// Ordered list of window IDs. Order determines window index (1-based in UI).
    pub windows: Vec<WindowId>,
    pub active_window: WindowId,
    pub env: HashMap<String, String>,
    pub theme: Option<ThemeRef>,
    pub tags: Tags,
    pub version: Version,
    /// Plugin that created this entity, if any. `None` for entities
    /// created by user CLI / RPC calls. Used by the permission model
    /// to grant a plugin authority over its own entities without an
    /// explicit grant (see `docs/designs/permissions/README.md` §5.2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by_plugin: Option<PluginId>,
}

impl Session {
    pub fn new(name: impl Into<String>, initial_window_id: WindowId) -> Self {
        Self {
            id: SessionId::new(),
            name: name.into(),
            created_at: SystemTime::now(),
            windows: vec![initial_window_id],
            active_window: initial_window_id,
            env: HashMap::new(),
            theme: None,
            tags: HashMap::new(),
            version: 1,
            created_by_plugin: None,
        }
    }
}

/// A window contains a layout tree of panes (PRD 5.1).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Window {
    pub id: WindowId,
    pub session_id: SessionId,
    pub title: String,
    pub active_pane: PaneId,
    pub layout: crate::layout::WindowLayout,
    pub cwd: Option<PathBuf>,
    pub theme: Option<ThemeRef>,
    pub tags: Tags,
    pub version: Version,
    /// Plugin that created this entity. See [`Session::created_by_plugin`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by_plugin: Option<PluginId>,
}

impl Window {
    /// Construct a window. The title is run through [`sanitize_title`] —
    /// the same rule pane titles use — so no construction site can mint a
    /// window whose title carries an escape sequence into the operator's
    /// terminal (issue #104).
    ///
    /// Sanitizing here may yield an **empty** title. Callers that need a
    /// non-empty one validate before constructing; see
    /// `SessionGraph::validate_window_title`, which is what every graph
    /// mutation path uses.
    pub fn new(session_id: SessionId, title: impl Into<String>, initial_pane_id: PaneId) -> Self {
        Self {
            id: WindowId::new(),
            session_id,
            title: sanitize_title(&title.into()),
            active_pane: initial_pane_id,
            layout: crate::layout::WindowLayout::new(initial_pane_id),
            cwd: None,
            theme: None,
            tags: HashMap::new(),
            version: 1,
            created_by_plugin: None,
        }
    }
}

/// A pane is a terminal viewport running a child process (PRD 5.1).
///
/// Title resolution priority (highest first), exposed via
/// [`Pane::effective_title`]:
///
/// 1. `manual_title` — explicitly set via `pane.set_title` RPC / `shux
///    pane title` CLI. Never overwritten by automatic sources.
/// 2. `osc_title` — set by the running app via OSC 0/2 escape
///    sequences (bash's `PROMPT_COMMAND` writes one of these per cwd
///    change; vim sets one per buffer).
/// 3. Auto-derived from `command` (first token, basename) or `cwd`.
/// 4. Empty string — never panic, fall back gracefully.
///
/// `auto_title = false` pins whatever was last computed and stops
/// future automatic updates (OSC + command). A subsequent
/// `set_manual_title(None)` re-enables auto.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pane {
    pub id: PaneId,
    pub window_id: WindowId,
    /// The currently-displayed title. Computed by `recalculate_title()`
    /// from the four sources above. Read directly by renderers (the
    /// compositor border draw doesn't need to know about priority).
    pub title: String,
    /// Set explicitly via API / CLI. Highest priority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manual_title: Option<String>,
    /// Set by the running app via OSC 0/2.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub osc_title: Option<String>,
    /// When true, automatic sources (OSC + command/cwd derivation)
    /// flow into `title`. When false, the current `title` is pinned.
    pub auto_title: bool,
    pub cwd: PathBuf,
    pub command: Vec<String>,
    pub exit_status: Option<i32>,
    pub restart: RestartPolicy,
    pub theme: Option<ThemeRef>,
    pub tags: Tags,
    pub version: Version,
    /// Plugin that created this entity. See [`Session::created_by_plugin`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by_plugin: Option<PluginId>,
}

impl Pane {
    pub fn new(window_id: WindowId, cwd: impl Into<PathBuf>) -> Self {
        let mut pane = Self {
            id: PaneId::new(),
            window_id,
            title: String::new(),
            manual_title: None,
            osc_title: None,
            auto_title: true,
            cwd: cwd.into(),
            command: Vec::new(),
            exit_status: None,
            restart: RestartPolicy::default(),
            theme: None,
            tags: HashMap::new(),
            version: 1,
            created_by_plugin: None,
        };
        pane.recalculate_title();
        pane
    }

    pub fn with_command(
        window_id: WindowId,
        cwd: impl Into<PathBuf>,
        command: Vec<String>,
    ) -> Self {
        let mut pane = Self::new(window_id, cwd);
        pane.command = command;
        pane.recalculate_title();
        pane
    }

    pub fn is_alive(&self) -> bool {
        self.exit_status.is_none()
    }

    pub fn should_restart(&self) -> bool {
        match (self.restart, self.exit_status) {
            (RestartPolicy::Always, Some(_)) => true,
            (RestartPolicy::OnFail, Some(code)) => code != 0,
            _ => false,
        }
    }

    /// Resolved title following the priority listed in the struct docs.
    /// Cheaper read than `&self.title` if you want to bypass any stale
    /// `title` cache, but `title` is kept in sync by `recalculate_title()`
    /// so direct reads are also fine.
    pub fn effective_title(&self) -> &str {
        if let Some(m) = self.manual_title.as_deref() {
            return m;
        }
        if self.auto_title
            && let Some(o) = self.osc_title.as_deref()
        {
            return o;
        }
        &self.title
    }

    /// Set or clear the manual title. Setting `None` lets automatic
    /// sources (OSC + command/cwd) flow back into `title`. Setting
    /// `Some` overrides them.
    pub fn set_manual_title(&mut self, title: Option<String>) {
        self.manual_title = title.map(|t| sanitize_title_clamped(&t));
        self.recalculate_title();
    }

    /// Record an OSC 0/2 title update from the running app. Returns
    /// `true` iff the new title actually changed the displayed
    /// `title` field (i.e. no manual override, auto enabled, value
    /// differs) — callers use this to decide whether to fire a
    /// `PaneTitleChanged` event without re-computing the priority.
    pub fn set_osc_title(&mut self, title: String) -> bool {
        let sanitized = sanitize_title_clamped(&title);
        let new_osc = if sanitized.is_empty() {
            None
        } else {
            Some(sanitized)
        };
        if self.osc_title == new_osc {
            return false;
        }
        self.osc_title = new_osc;
        let old = self.title.clone();
        self.recalculate_title();
        old != self.title
    }

    /// Toggle the auto-title flag. When turning OFF, the current
    /// title is pinned (re-derivation stops). When turning ON, the
    /// priority resolution kicks back in. Callers should fire a
    /// `PaneTitleChanged` event if the displayed title changes.
    pub fn set_auto_title(&mut self, enabled: bool) {
        if self.auto_title == enabled {
            return;
        }
        self.auto_title = enabled;
        self.recalculate_title();
    }

    /// Recompute `self.title` from the priority sources. Called
    /// internally on every mutation that could affect display.
    pub(crate) fn recalculate_title(&mut self) {
        if let Some(m) = &self.manual_title {
            self.title = m.clone();
            return;
        }
        if self.auto_title {
            if let Some(o) = &self.osc_title {
                self.title = o.clone();
                return;
            }
            // Auto from the command's program name, or the cwd basename.
            //
            // These are as untrusted as the OSC and manual sources: a
            // template picks the argv and the cwd, and neither is
            // validated as an existing path. They go through the same
            // sanitizer, or `sanitize_title` is not "the single title
            // rule" it claims to be (issue #104).
            // A name that sanitizes to NOTHING is not a title. Round three
            // widened both what counts as a program name and what the sanitizer
            // removes, so `--cmd "<soft hyphen>"` produced a pane with a blank
            // border and a blank status bar. Fall through to the cwd instead.
            if let Some(name) = command_display_name(&self.command) {
                let sanitized = sanitize_title_clamped(name);
                if !sanitized.is_empty() {
                    self.title = sanitized;
                    return;
                }
            }
            if let Some(name) = self.cwd.file_name().and_then(|s| s.to_str()) {
                let sanitized = sanitize_title_clamped(name);
                if !sanitized.is_empty() {
                    self.title = sanitized;
                    return;
                }
            }
            // The cwd can fail to yield a name for two ordinary reasons, and
            // both left the pane with a blank border and a blank status bar —
            // the outcome the command fallback above was added to prevent.
            //
            // `Path::file_name` is `None` for a root path, so plain
            // `shux session create s --cwd /` produced no title at all. And a
            // directory whose own name sanitizes away leaves nothing either.
            // The whole path is the honest answer for the first case; for the
            // second there is no name to show, so say what the thing is.
            let whole = sanitize_title_clamped(&self.cwd.to_string_lossy());
            self.title = if whole.is_empty() {
                FALLBACK_PANE_TITLE.to_string()
            } else {
                whole
            };
        }
        // Auto disabled and no manual override → keep whatever we had.
    }
}

/// Shown when neither the command nor the cwd yields a printable name.
/// A pane always has a title — "no title" is a rendering bug, not a state.
const FALLBACK_PANE_TITLE: &str = "pane";

/// The program name to show for a pane running `command`.
///
/// The obvious answer — the basename of `command[0]` — is the right one for an
/// argv like `["nvim", "src/main.rs"]`. It is the wrong one whenever the argv is
/// a **shell wrapper**, which is now the normal shape for a pane started from a
/// shell command string (`shux session create --cmd "npm run dev"` runs
/// `$SHELL -c "npm run dev"`, issue #125) and has always been the shape of the
/// documented escape hatch `-- sh -c "npm run dev"`. Titling those panes `bash`
/// and `sh` tells the operator nothing: every one of them looks the same.
///
/// So a wrapper is unwrapped and the title comes from the first real word of the
/// script — skipping an `exec` prefix and any leading `NAME=value` assignments,
/// the two things that routinely sit in front of the actual program. When the
/// script does not begin with a plain command word (a subshell, a redirection, a
/// `for` loop) there is no honest short answer and the shell's own name is used.
///
/// ```text
/// ["nvim", "a.rs"]                    -> nvim
/// ["/bin/bash", "-lc", "npm run dev"] -> npm
/// ["sh", "-c", "exec TERM=x top"]     -> top
/// ["sh", "-c", "(cd x && make)"]      -> sh
/// ["/usr/local/bin/my-c", "-c", "x"]  -> my-c   (not a shell; not unwrapped)
/// ```
///
/// The shell test is on the **basename**, so a program that is genuinely not a
/// shell but happens to be installed as `sh` or `bash` somewhere on `PATH` is
/// unwrapped too, and its title then names the script's first word rather than
/// the program that is running. Matching on absolute path instead would be
/// worse: it would miss every real shell outside the handful of paths worth
/// hard-coding.
pub(crate) fn command_display_name(command: &[String]) -> Option<&str> {
    let raw = command.first()?;
    let program = program_name(raw);
    if let Some(program) = program
        && is_shell(program)
        && command.len() >= 3
        && is_shell_command_flag(&command[1])
        && let Some(word) = script_leading_word(&command[2])
    {
        return Some(word);
    }
    // argv[0] gets the same plausibility test as a script's first word: a token
    // that names no program is not a title, and the cwd basename below is a
    // better answer than `/` or `..`.
    program
}

/// The file name inside `path`, or `None` when there is not one — `/`, `//`,
/// `.` and `..` all resolve to nothing a pane can be named after.
fn program_name(path: &str) -> Option<&str> {
    let name = std::path::Path::new(path).file_name()?.to_str()?;
    (!name.is_empty()).then_some(name)
}

/// Shells whose `-c` takes a script as one argument. Deliberately a fixed list:
/// guessing from the argument shape would unwrap `openssl -c ...` too.
fn is_shell(program: &str) -> bool {
    matches!(
        program,
        "sh" | "bash" | "zsh" | "dash" | "ash" | "ksh" | "ksh93" | "mksh" | "fish" | "busybox"
    )
}

/// `-c` and the login/interactive variants shells accept as one cluster:
/// `-c`, `-lc`, `-ic`, `-lic`, `-cl`, … Anything else (including `--command`,
/// which is a different, non-shell convention) is not a wrapper.
fn is_shell_command_flag(flag: &str) -> bool {
    let Some(letters) = flag.strip_prefix('-') else {
        return false;
    };
    !letters.is_empty()
        && letters.contains('c')
        && letters.chars().all(|c| matches!(c, 'c' | 'l' | 'i'))
}

/// The first word of a shell script that names the program it runs, or `None`
/// when the script does not start with one.
///
/// Only the script's **first simple command** is considered — everything up to
/// the first shell operator. Reading past one is how `A=1;htop -d 10` produced
/// the title `-d`: the leading token is a complete assignment, and the scanner
/// walked on into a *flag* belonging to a command it never established.
fn script_leading_word(script: &str) -> Option<&str> {
    let segment = script.split(is_shell_operator).next()?;
    for token in segment.split_whitespace() {
        // `exec top` and `time make` run `top` and `make`.
        if token == "exec" || token == "time" || is_env_assignment(token) {
            continue;
        }
        // `for`, `if`, `while` … introduce a compound command. There is no
        // single program to name, and naming the loop variable would be worse
        // than saying nothing.
        if is_shell_keyword(token) || token.chars().any(is_shell_syntax) {
            return None;
        }
        // A leading `-` is a flag, and a flag is never the program. Without this
        // `--cmd "-n is a valid sed script"` — the example in the flag's own
        // help — titled the pane `-n`, and `A=1 -d 10` titled it `-d`.
        if token.starts_with('-') {
            return None;
        }
        // `/`, `//`, `.` and `..` have no file name in them at all; `Path`
        // says so, and that is the right test. Judging by the glyphs instead —
        // "no alphanumeric character" — also rejected `/usr/local/bin/+++`,
        // which is a real program with an unusual name.
        let name = program_name(token)?;
        // A bare number is a file descriptor (`2>&1 make`), never a program.
        if name.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        return Some(name);
    }
    None
}

/// Characters that end a simple command. Splitting on these first is what keeps
/// `ls|wc` readable as `ls` while `(cd x && make)` correctly yields nothing.
fn is_shell_operator(c: char) -> bool {
    matches!(c, '|' | '&' | ';' | '<' | '>' | '(' | ')' | '`' | '\n')
}

/// Reserved words that open a compound command.
fn is_shell_keyword(token: &str) -> bool {
    matches!(
        token,
        "if" | "then"
            | "else"
            | "elif"
            | "fi"
            | "for"
            | "while"
            | "until"
            | "do"
            | "done"
            | "case"
            | "esac"
            | "in"
            | "select"
            | "function"
            | "coproc"
            | ":"
            | "{"
            | "}"
            | "[["
            | "]]"
    )
}

/// `NAME=value` in the leading position — a per-command environment override,
/// not the program.
///
/// The **value** is checked too. `A=1;htop` reaches here only if the operator
/// split above missed it, and a value carrying `$`, a quote or a glob is not a
/// plain assignment this code can reason about.
fn is_env_assignment(token: &str) -> bool {
    let Some((name, value)) = token.split_once('=') else {
        return false;
    };
    let name_ok = !name.is_empty()
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    // '=' is itself in `is_shell_syntax`; `A=b=c` is a legal assignment.
    name_ok && !value.chars().any(|c| c != '=' && is_shell_syntax(c))
}

/// Characters that mean a token is shell syntax rather than a program name.
/// The command *operators* are not here — [`is_shell_operator`] has already
/// removed them by the time a token is examined.
fn is_shell_syntax(c: char) -> bool {
    matches!(
        c,
        '$' | '\\' | '"' | '\'' | '*' | '?' | '[' | ']' | '{' | '}' | '~' | '#' | '!' | '='
    )
}

/// Characters that must never survive into a stored title.
///
/// A title is (a) drawn into **one row** of border chrome and (b) echoed
/// to the operator's terminal by the CLI, so it has to be a single line of
/// inert text. Three classes break that:
///
/// - **`char::is_control()`** — C0, DEL and C1. The reported vector
///   (issue #104) is a C0 `ESC` opening an OSC set-title payload, but C1
///   matters just as much: a terminal in 8-bit mode reads `U+009B` as CSI
///   and `U+009D` as OSC with no `ESC` anywhere in sight.
/// - **`U+2028` / `U+2029`** — line and paragraph separators. Not
///   `is_control()`, but they end a line in exactly the place the
///   border-draw code assumes there is none.
/// - **`U+202A`–`U+202E`, `U+2066`–`U+2069`, `U+200E`, `U+200F`,
///   `U+061C`** — bidi embedding, override, isolate and mark formatting.
///   These reorder the *rendered* title without changing its bytes, which
///   is how a title spoofs another one (the Trojan Source class,
///   CVE-2021-42574). This is the same set `rustc`'s
///   `text_direction_codepoint_in_literal` lint covers. Only the explicit
///   formatting characters are dropped; the implicit bidi algorithm is
///   untouched, so ordinary RTL titles still render correctly.
///
/// - **Default-ignorable code points** — invisible, not `is_control()`, and far
///   more numerous than the handful anyone names first. `ht<ZWSP>op` renders
///   identically to `htop`, so two panes carry titles a human cannot tell
///   apart; so do soft hyphen, the combining grapheme joiner, the invisible
///   operators, the Hangul fillers, the variation selectors and the tag block.
///   The first cut of this listed three of them and let fifteen more through —
///   an allowlist where the property is what matters. See [`is_default_ignorable`].
///   It is not only an operator's problem: a program running in a pane sets its
///   own title with OSC 0, so it can mint an indistinguishable one unaided.
///
/// Deliberately **not** dropped: `U+200D` ZERO WIDTH JOINER, which is
/// load-bearing inside emoji sequences — removing it would split a
/// perfectly legitimate title into separate glyphs.
pub(crate) fn is_title_hostile(c: char) -> bool {
    c.is_control()
        || matches!(c, '\u{2028}' | '\u{2029}')
        || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        || matches!(c, '\u{200e}' | '\u{200f}' | '\u{061c}')
        || is_default_ignorable(c)
}

/// Unicode's `Default_Ignorable_Code_Point` set, minus the one exception below.
///
/// These render as nothing. Two titles differing only in these characters are
/// the same title to every human who looks at them, which is the whole of the
/// spoof — no reordering required, unlike the bidi set. Enumerated as ranges
/// rather than as the handful people think of first, because the property is
/// what matters and the handful is never complete.
///
/// **Three are kept**, for the same reason: they compose sequences a person
/// legitimately types, and dropping them changes what the title *says*.
///
/// - `U+200D` ZERO WIDTH JOINER — `👨‍👩‍👧` becomes three separate people.
/// - `U+FE0F` VARIATION SELECTOR-16 (emoji presentation) and `U+FE0E` (text
///   presentation). VS16 is mandatory in RGI keycap and several RGI ZWJ
///   sequences: stripping it turned `❤️‍🔥` into `❤`, `1️⃣` into a bare `1`, and
///   `⚠️` into `⚠`. The first cut of this list included them, which broke the
///   very sequences the ZWJ exception exists to protect.
///
/// That is three spoofable characters, knowingly, against a rule that would
/// otherwise corrupt ordinary titles.
fn is_default_ignorable(c: char) -> bool {
    if matches!(c, '\u{200d}' | '\u{fe0e}' | '\u{fe0f}') {
        return false; // composing selectors — see above.
    }
    matches!(c,
        '\u{00ad}'                        // SOFT HYPHEN
        | '\u{034f}'                      // COMBINING GRAPHEME JOINER
        | '\u{115f}'..='\u{1160}'         // HANGUL CHOSEONG/JUNGSEONG FILLER
        | '\u{17b4}'..='\u{17b5}'         // KHMER INHERENT VOWELS
        | '\u{180b}'..='\u{180f}'         // MONGOLIAN FVS + VOWEL SEPARATOR
        | '\u{200b}'..='\u{200f}'         // ZWSP, ZWNJ, (ZWJ), LRM, RLM
        | '\u{202a}'..='\u{202e}'         // bidi embedding / override
        | '\u{2060}'..='\u{206f}'         // word joiner, invisible operators, deprecated format
        | '\u{3164}'                      // HANGUL FILLER
        | '\u{fe00}'..='\u{fe0f}'         // VARIATION SELECTOR 1..16 (VS15/16 exempt above)
        | '\u{feff}'                      // ZERO WIDTH NO-BREAK SPACE / BOM
        | '\u{ffa0}'                      // HALFWIDTH HANGUL FILLER
        | '\u{fff0}'..='\u{fff8}'         // unassigned, reserved as ignorable
        | '\u{1bca0}'..='\u{1bca3}'       // SHORTHAND FORMAT CONTROLS
        | '\u{1d173}'..='\u{1d17a}'       // MUSICAL SYMBOL beams/slurs (format)
        | '\u{e0000}'..='\u{e0fff}'       // TAGS + VARIATION SELECTORS SUPPLEMENT
    )
}

/// The longest title a pane will display. The border has limited room
/// and a very long title squeezes out the rest of the chrome.
pub const MAX_TITLE_CHARS: usize = 64;

/// Neutralize a title: drop every [`is_title_hostile`] character, then
/// trim.
///
/// **The single title rule for panes and windows alike** — pane manual
/// titles ([`Pane::set_manual_title`]), pane OSC titles
/// ([`Pane::set_osc_title`]), auto-derived pane titles, and window titles
/// ([`Window::new`] plus every `SessionGraph` window-title ingress) all
/// funnel through here, so the two entity kinds cannot drift apart
/// (issue #104).
///
/// This does **not** shorten the title. Length is a display concern and
/// the two entity kinds want different answers: a pane title is pure
/// chrome, so it is clamped ([`sanitize_title_clamped`]); a **window**
/// title is also a *lookup key* — `window.ensure` is idempotent by name
/// and `shux window … -w <name>` selects by it — and silently truncating
/// a lookup key makes two distinct requested names resolve to one window,
/// so over-long window titles are rejected instead
/// (`SessionGraph::validate_window_title`).
///
/// The result may be **empty** (a title made entirely of hostile
/// characters collapses). Callers that require a non-empty title
/// sanitize first and validate second.
pub fn sanitize_title(raw: &str) -> String {
    raw.chars()
        .filter(|c| !is_title_hostile(*c))
        .collect::<String>()
        .trim()
        .to_string()
}

/// [`sanitize_title`] plus the [`MAX_TITLE_CHARS`] display clamp, for
/// pane titles.
///
/// The strip runs **before** the clamp so an attacker cannot push a
/// payload past the window behind filler that later disappears. The clamp
/// counts characters, so a multi-byte title is never cut mid-scalar.
pub fn sanitize_title_clamped(raw: &str) -> String {
    let cleaned = sanitize_title(raw);
    if cleaned.chars().count() <= MAX_TITLE_CHARS {
        cleaned
    } else {
        cleaned.chars().take(MAX_TITLE_CHARS).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_id_uniqueness() {
        let a = SessionId::new();
        let b = SessionId::new();
        assert_ne!(a, b);
    }

    #[test]
    fn test_id_copy_and_eq() {
        let a = SessionId::new();
        let b = a;
        assert_eq!(a, b);
    }

    #[test]
    fn test_id_display() {
        let id = SessionId::new();
        let s = id.to_string();
        assert!(!s.is_empty());
        // UUID v4 format
        assert_eq!(s.len(), 36);
    }

    #[test]
    fn test_id_from_uuid() {
        let uuid = Uuid::new_v4();
        let id = PaneId::from_uuid(uuid);
        assert_eq!(*id.as_uuid(), uuid);
    }

    #[test]
    fn test_id_serialize_roundtrip() {
        let id = WindowId::new();
        let json = serde_json::to_string(&id).unwrap();
        let deserialized: WindowId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, deserialized);
    }

    #[test]
    fn test_session_new() {
        let wid = WindowId::new();
        let session = Session::new("work", wid);
        assert_eq!(session.name, "work");
        assert_eq!(session.windows, vec![wid]);
        assert_eq!(session.active_window, wid);
        assert_eq!(session.version, 1);
    }

    #[test]
    fn test_window_new() {
        let sid = SessionId::new();
        let pid = PaneId::new();
        let window = Window::new(sid, "editor", pid);
        assert_eq!(window.session_id, sid);
        assert_eq!(window.title, "editor");
        assert_eq!(window.active_pane, pid);
    }

    #[test]
    fn test_pane_new() {
        let wid = WindowId::new();
        let pane = Pane::new(wid, "/home/test");
        assert_eq!(pane.window_id, wid);
        assert!(pane.is_alive());
        assert!(!pane.should_restart());
    }

    #[test]
    fn test_pane_with_command() {
        let wid = WindowId::new();
        let pane = Pane::with_command(wid, "/home/test", vec!["vim".into()]);
        assert_eq!(pane.command, vec!["vim"]);
    }

    #[test]
    fn test_pane_restart_policy() {
        let wid = WindowId::new();
        let mut pane = Pane::new(wid, "/home/test");

        // Never restart (default)
        pane.exit_status = Some(1);
        assert!(!pane.should_restart());

        // OnFail with failure
        pane.restart = RestartPolicy::OnFail;
        pane.exit_status = Some(1);
        assert!(pane.should_restart());

        // OnFail with success
        pane.exit_status = Some(0);
        assert!(!pane.should_restart());

        // Always
        pane.restart = RestartPolicy::Always;
        pane.exit_status = Some(0);
        assert!(pane.should_restart());

        // Still running
        pane.exit_status = None;
        assert!(!pane.should_restart());
    }

    #[test]
    fn test_restart_policy_serde() {
        let json = serde_json::to_string(&RestartPolicy::OnFail).unwrap();
        assert_eq!(json, "\"on-fail\"");
        let deserialized: RestartPolicy = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, RestartPolicy::OnFail);
    }

    // ── PR 4 / task 027: pane title resolution ────────────────────────

    #[test]
    fn test_pane_auto_title_derives_from_command() {
        let wid = WindowId::new();
        let pane = Pane::with_command(wid, "/home/test", vec!["vim".into(), "foo.rs".into()]);
        // First-arg basename → "vim".
        assert_eq!(pane.effective_title(), "vim");
        assert_eq!(pane.title, "vim");
    }

    #[test]
    fn test_pane_auto_title_takes_basename_of_command() {
        let wid = WindowId::new();
        let pane = Pane::with_command(wid, "/home/test", vec!["/usr/bin/htop".into()]);
        assert_eq!(pane.effective_title(), "htop");
    }

    // ── issue #125: a shell wrapper must not become the title ─────────
    //
    // `--cmd "npm run dev"` runs `$SHELL -c "npm run dev"`. Titling the pane
    // after the shell would make every `--cmd` pane in a session look
    // identical.

    #[track_caller]
    fn title_of(argv: &[&str]) -> String {
        let pane = Pane::with_command(
            WindowId::new(),
            "/home/test/proj",
            argv.iter().map(|s| s.to_string()).collect(),
        );
        pane.effective_title().to_string()
    }

    #[test]
    fn shell_wrapper_titles_after_the_program_the_script_runs() {
        assert_eq!(title_of(&["/bin/bash", "-c", "npm run dev"]), "npm");
        assert_eq!(title_of(&["sh", "-c", "top"]), "top");
        assert_eq!(title_of(&["/usr/bin/zsh", "-lc", "cargo watch"]), "cargo");
        assert_eq!(title_of(&["dash", "-ic", "htop"]), "htop");
        assert_eq!(title_of(&["fish", "-lic", "btop"]), "btop");
    }

    #[test]
    fn shell_wrapper_skips_exec_and_environment_prefixes() {
        assert_eq!(title_of(&["sh", "-c", "exec top"]), "top");
        assert_eq!(title_of(&["sh", "-c", "RUST_LOG=debug cargo run"]), "cargo");
        assert_eq!(
            title_of(&["sh", "-c", "exec FOO=1 BAR=2 btop -p 1"]),
            "btop"
        );
    }

    #[test]
    fn shell_wrapper_takes_the_basename_of_an_absolute_program() {
        assert_eq!(
            title_of(&["sh", "-c", "/usr/local/bin/lazygit -p ."]),
            "lazygit"
        );
    }

    #[test]
    fn shell_wrapper_falls_back_to_the_shell_when_the_script_is_not_a_plain_command() {
        // A subshell, a redirection, a variable: no honest short name.
        assert_eq!(title_of(&["sh", "-c", "(cd x && make)"]), "sh");
        assert_eq!(title_of(&["bash", "-c", "> log 2>&1 make"]), "bash");
        assert_eq!(title_of(&["sh", "-c", "$EDITOR notes"]), "sh");
        assert_eq!(title_of(&["sh", "-c", "   "]), "sh");
        assert_eq!(title_of(&["sh", "-c", ""]), "sh");
        // Only assignments, no command.
        assert_eq!(title_of(&["sh", "-c", "FOO=1"]), "sh");
        // A leading file descriptor is not a program.
        assert_eq!(title_of(&["sh", "-c", "2>&1 make"]), "sh");
    }

    /// A compound command has no single program to name. Naming the loop
    /// variable or the branch keyword would be worse than saying nothing.
    #[test]
    fn shell_keywords_are_not_titles() {
        assert_eq!(
            title_of(&["sh", "-c", "for i in 1 2; do echo $i; done"]),
            "sh"
        );
        assert_eq!(title_of(&["sh", "-c", "if true; then htop; fi"]), "sh");
        assert_eq!(
            title_of(&["bash", "-c", "while true; do htop; done"]),
            "bash"
        );
        assert_eq!(title_of(&["sh", "-c", "until false; do :; done"]), "sh");
        assert_eq!(title_of(&["sh", "-c", "case $x in a) :; esac"]), "sh");
        assert_eq!(title_of(&["bash", "-c", "{ htop; }"]), "bash");
    }

    /// Only the FIRST simple command is read. Walking past a complete leading
    /// assignment is how `A=1;htop -d 10` came out titled `-d` — a flag
    /// belonging to a command the scanner never established.
    #[test]
    fn only_the_first_simple_command_is_considered() {
        assert_eq!(title_of(&["bash", "-c", "A=1;htop -d 10"]), "bash");
        assert_eq!(title_of(&["bash", "-c", "A=1;htop"]), "bash");
        assert_eq!(title_of(&["bash", "-c", "cd /x && make"]), "cd");
        assert_eq!(title_of(&["bash", "-c", "make; htop"]), "make");
    }

    /// Spacing around an operator must not change the answer.
    #[test]
    fn an_operator_without_surrounding_spaces_still_ends_the_command() {
        assert_eq!(title_of(&["bash", "-c", "ls|wc"]), "ls");
        assert_eq!(title_of(&["bash", "-c", "ls | wc"]), "ls");
        assert_eq!(title_of(&["bash", "-c", "make&&test"]), "make");
        assert_eq!(title_of(&["bash", "-c", "npm run dev>log"]), "npm");
    }

    #[test]
    fn time_is_skipped_like_exec() {
        assert_eq!(title_of(&["sh", "-c", "time make -j4"]), "make");
    }

    /// A flag is never the program. `--cmd "-n is a valid sed script"` — the
    /// example printed in the flag's own help — used to title the pane `-n`.
    /// An invisible character makes two different titles look identical. The
    /// first cut named three of them; these are the ones it let through.
    #[test]
    fn test_sanitize_title_strips_invisible_spacers() {
        let ignorable = [
            '\u{00ad}',
            '\u{034f}',
            '\u{115f}',
            '\u{1160}',
            '\u{17b4}',
            '\u{180e}',
            '\u{200b}',
            '\u{200c}',
            '\u{200e}',
            '\u{2060}',
            '\u{2061}',
            '\u{2062}',
            '\u{2063}',
            '\u{2064}',
            '\u{3164}',
            '\u{fe00}',
            '\u{feff}',
            '\u{ffa0}',
            '\u{e0041}',
        ];
        for c in ignorable {
            assert_eq!(
                sanitize_title(&format!("ht{c}op")),
                "htop",
                "U+{:04X} survived the sanitizer",
                c as u32
            );
            // …and through the title pipeline a running program can drive.
            let mut pane = Pane::new(WindowId::new(), "/home/test/proj");
            pane.set_osc_title(format!("ht{c}op"));
            assert_eq!(pane.effective_title(), "htop", "U+{:04X} via OSC", c as u32);
        }
        // The joiner is load-bearing inside emoji and is the one exception.
        assert!(sanitize_title("a\u{200d}b").contains('\u{200d}'));
    }

    /// Sequences a person legitimately types must survive intact. The first
    /// cut of the ignorable-codepoint rule stripped the variation selectors,
    /// which are mandatory in the very emoji the ZWJ exception protects.
    #[test]
    fn test_sanitize_title_keeps_composing_selectors() {
        for (input, why) in [
            ("\u{2764}\u{fe0f}\u{200d}\u{1f525}", "heart on fire"),
            ("\u{1f3f3}\u{fe0f}\u{200d}\u{1f308}", "rainbow flag"),
            ("1\u{fe0f}\u{20e3}", "keycap one"),
            ("\u{26a0}\u{fe0f}", "warning sign"),
            ("\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}", "family"),
        ] {
            assert_eq!(sanitize_title(input), input, "{why} was altered");
        }
    }

    /// A name that sanitizes to nothing is not a title — the cwd is.
    #[test]
    fn a_command_whose_name_sanitizes_to_nothing_falls_through_to_the_cwd() {
        for invisible in ["\u{00ad}", "\u{2060}", "\u{3164}"] {
            let pane = Pane::with_command(
                WindowId::new(),
                "/home/test/proj",
                vec![invisible.to_string()],
            );
            assert_eq!(
                pane.effective_title(),
                "proj",
                "{invisible:?} left a blank title"
            );
        }
    }

    /// …and when the cwd cannot supply one either, something still has to.
    ///
    /// Both of these are ordinary invocations, not adversarial input:
    /// `Path::file_name` is `None` for a root path, and a directory can be
    /// named with characters the sanitizer removes. Each used to leave the
    /// pane with a blank border and a blank status bar.
    #[test]
    fn a_pane_always_has_a_title() {
        let root = Pane::new(WindowId::new(), "/");
        assert_eq!(root.effective_title(), "/", "a root cwd left no title");

        let root_cmd = Pane::with_command(WindowId::new(), "/", vec!["\u{00ad}".to_string()]);
        assert_eq!(
            root_cmd.effective_title(),
            "/",
            "a blank command name over a root cwd left no title"
        );

        // Nothing printable anywhere: not the command, not the directory's
        // name, not the whole path.
        let nameless =
            Pane::with_command(WindowId::new(), "\u{00ad}", vec!["\u{00ad}".to_string()]);
        assert_eq!(
            nameless.effective_title(),
            "pane",
            "a pane with nothing printable anywhere left no title"
        );
    }

    #[test]
    fn a_flag_is_never_a_title() {
        assert_eq!(
            title_of(&["bash", "-c", "-n is a valid sed script"]),
            "bash"
        );
        assert_eq!(title_of(&["bash", "-c", "--format json"]), "bash");
        assert_eq!(title_of(&["bash", "-c", "-d"]), "bash");
        // The space route past a leading assignment, which the `;` route caught
        // but this one did not.
        assert_eq!(title_of(&["bash", "-c", "A=1 -d 10"]), "bash");
        assert_eq!(title_of(&["sh", "-c", "-- htop"]), "sh");
    }

    /// `basename` returns the whole token when there is no file name in it, so
    /// a path-shaped non-program used to become the title verbatim.
    /// `/`, `//`, `.` and `..` contain no file name, so there is nothing to
    /// name the pane after.
    #[test]
    fn a_token_with_no_program_name_in_it_is_not_a_title() {
        assert_eq!(title_of(&["sh", "-c", "/"]), "sh");
        assert_eq!(title_of(&["sh", "-c", "//"]), "sh");
        assert_eq!(title_of(&["sh", "-c", "."]), "sh");
        assert_eq!(title_of(&["sh", "-c", ".."]), "sh");
        // …but a real program with an unusual name keeps it. Judging by the
        // glyphs ("no alphanumeric character") threw this away too.
        assert_eq!(title_of(&["sh", "-c", "/usr/local/bin/+++ arg"]), "+++");
        assert_eq!(title_of(&["/usr/local/bin/+++"]), "+++");
        assert_eq!(title_of(&["sh", "-c", "+++ arg"]), "+++");
    }

    /// argv[0] is judged the same way a script's first word is; when it names
    /// no program the cwd basename is a better title than `/` or `..`.
    #[test]
    fn an_argv_that_names_no_program_falls_through_to_the_cwd() {
        assert_eq!(title_of(&["/"]), "proj");
        assert_eq!(title_of(&[".."]), "proj");
    }

    #[test]
    fn a_program_that_merely_takes_dash_c_is_not_unwrapped() {
        // `is_shell` is a fixed list precisely so this does not happen.
        assert_eq!(title_of(&["openssl", "-c", "req"]), "openssl");
        assert_eq!(title_of(&["/opt/tool/shush", "-c", "start"]), "shush");
    }

    #[test]
    fn a_shell_without_a_dash_c_script_titles_after_the_shell() {
        assert_eq!(title_of(&["bash", "-l", "-i"]), "bash");
        assert_eq!(title_of(&["sh"]), "sh");
        assert_eq!(title_of(&["sh", "-c"]), "sh"); // truncated wrapper
        assert_eq!(title_of(&["bash", "--command", "top"]), "bash");
        assert_eq!(title_of(&["bash", "-x", "top"]), "bash");
    }

    #[test]
    fn an_unwrapped_script_word_is_still_sanitized() {
        // The script is as untrusted as any other title source (issue #104).
        let title = title_of(&["sh", "-c", "ev\u{1b}]0;spoof\u{7}il --now"]);
        assert!(!title.contains('\u{1b}'), "{title:?}");
        assert!(!title.contains('\u{7}'), "{title:?}");
    }

    #[test]
    fn test_pane_auto_title_falls_back_to_cwd_basename() {
        let wid = WindowId::new();
        let pane = Pane::new(wid, "/home/test/projects/myproj");
        assert_eq!(pane.effective_title(), "myproj");
    }

    #[test]
    fn test_pane_manual_title_overrides_command_derived() {
        let wid = WindowId::new();
        let mut pane = Pane::with_command(wid, "/home/test", vec!["vim".into()]);
        pane.set_manual_title(Some("notes".into()));
        // Manual wins over the command-derived auto.
        assert_eq!(pane.effective_title(), "notes");
        assert_eq!(pane.title, "notes");
    }

    #[test]
    fn test_pane_osc_title_overrides_command_derived_when_auto() {
        let wid = WindowId::new();
        let mut pane = Pane::with_command(wid, "/home/test", vec!["bash".into()]);
        let changed = pane.set_osc_title("~/work/x".into());
        assert!(changed);
        assert_eq!(pane.effective_title(), "~/work/x");
    }

    #[test]
    fn test_pane_manual_title_beats_osc_title() {
        let wid = WindowId::new();
        let mut pane = Pane::new(wid, "/home/test");
        pane.set_osc_title("from-osc".into());
        pane.set_manual_title(Some("manual".into()));
        // Both set → manual priority.
        assert_eq!(pane.effective_title(), "manual");
        // Clearing manual lets OSC flow back.
        pane.set_manual_title(None);
        assert_eq!(pane.effective_title(), "from-osc");
    }

    #[test]
    fn test_pane_auto_title_off_pins_current_title() {
        let wid = WindowId::new();
        let mut pane = Pane::with_command(wid, "/home/test", vec!["bash".into()]);
        assert_eq!(pane.title, "bash");
        pane.set_auto_title(false);
        // Subsequent OSC updates must NOT change the displayed title.
        let changed = pane.set_osc_title("changed".into());
        // osc_title field still records the value (so re-enabling auto
        // picks it up), but title stays pinned.
        assert!(!changed);
        assert_eq!(pane.title, "bash");
        // Re-enabling auto pulls the recorded osc_title into title.
        pane.set_auto_title(true);
        assert_eq!(pane.title, "changed");
    }

    #[test]
    fn test_pane_osc_title_idempotent() {
        let wid = WindowId::new();
        let mut pane = Pane::with_command(wid, "/home/test", vec!["bash".into()]);
        let first = pane.set_osc_title("same".into());
        let second = pane.set_osc_title("same".into());
        assert!(first, "first call must report a change");
        assert!(!second, "second call with same value must report no change");
    }

    #[test]
    fn test_pane_sanitize_title_strips_control_chars() {
        // BEL (0x07), ESC (0x1b), LF (0x0a) are control bytes and
        // must be dropped. The closing `]` is a printable ASCII char
        // and survives — sanitize_title only drops `c.is_control()`,
        // it doesn't try to strip OSC syntax. (Border-draw code
        // displays the result one char per cell, so as long as no
        // control byte slips through, we're safe.)
        let wid = WindowId::new();
        let mut pane = Pane::new(wid, "/home/test");
        pane.set_manual_title(Some("hello\x07\x1b]world\n".into()));
        assert_eq!(pane.title, "hello]world");
    }

    #[test]
    fn test_pane_sanitize_title_clamps_to_64_chars() {
        let wid = WindowId::new();
        let mut pane = Pane::new(wid, "/home/test");
        let long: String = "x".repeat(120);
        pane.set_manual_title(Some(long));
        assert_eq!(pane.title.chars().count(), 64);
    }

    // ── issue #104: one shared title sanitizer for panes AND windows ──

    /// The reported vector, at the sanitizer: a TOML ``/``
    /// pair decodes to real ESC/BEL bytes before shux sees them.
    #[test]
    fn test_sanitize_title_strips_osc_set_title_payload() {
        let out = sanitize_title("\u{1b}]0;attacker-controlled\u{7}deploy");
        assert_eq!(out, "]0;attacker-controlleddeploy");
        assert!(
            !out.chars().any(|c| c.is_control()),
            "no control byte may survive: {out:?}"
        );
    }

    /// C0, DEL and C1 all have to go. C1 (0x80..=0x9F) matters because a
    /// terminal in 8-bit mode treats 0x9B as CSI and 0x9D as OSC with no
    /// ESC in sight.
    #[test]
    fn test_sanitize_title_strips_c0_del_and_c1() {
        for (label, ch) in [
            ("NUL", '\u{0}'),
            ("BEL", '\u{7}'),
            ("BS", '\u{8}'),
            ("TAB", '\u{9}'),
            ("LF", '\u{a}'),
            ("CR", '\u{d}'),
            ("ESC", '\u{1b}'),
            ("DEL", '\u{7f}'),
            ("C1-CSI", '\u{9b}'),
            ("C1-OSC", '\u{9d}'),
            ("C1-PAD", '\u{80}'),
            ("C1-APC", '\u{9f}'),
        ] {
            let out = sanitize_title(&format!("a{ch}b"));
            assert_eq!(out, "ab", "{label} (U+{:04X}) survived", ch as u32);
        }
    }

    /// A title is a single line drawn into one row of border chrome, and
    /// it names a thing the operator makes trust decisions about. Line
    /// separators and bidi overrides break both invariants without ever
    /// being `char::is_control()`.
    #[test]
    fn test_sanitize_title_strips_separators_and_bidi_overrides() {
        for (label, ch) in [
            ("LINE SEPARATOR", '\u{2028}'),
            ("PARAGRAPH SEPARATOR", '\u{2029}'),
            ("LRE", '\u{202a}'),
            ("RLE", '\u{202b}'),
            ("PDF", '\u{202c}'),
            ("LRO", '\u{202d}'),
            ("RLO", '\u{202e}'),
            ("LRI", '\u{2066}'),
            ("RLI", '\u{2067}'),
            ("FSI", '\u{2068}'),
            ("PDI", '\u{2069}'),
        ] {
            let out = sanitize_title(&format!("a{ch}b"));
            assert_eq!(out, "ab", "{label} (U+{:04X}) survived", ch as u32);
        }
    }

    /// Ordinary RTL text must still work — we drop the explicit override
    /// characters, not the script.
    #[test]
    fn test_sanitize_title_keeps_ordinary_unicode() {
        assert_eq!(sanitize_title("مرحبا"), "مرحبا");
        assert_eq!(sanitize_title("日本語 セッション"), "日本語 セッション");
        assert_eq!(sanitize_title("build ✓ 🚀"), "build ✓ 🚀");
    }

    /// A title made only of hostile bytes collapses to empty. Callers
    /// rely on this to reject rather than store `""`.
    #[test]
    fn test_sanitize_title_all_hostile_collapses_to_empty() {
        assert_eq!(sanitize_title("\u{1b}\u{7}"), "");
        assert_eq!(sanitize_title("\u{9b}\u{202e}\u{2028}"), "");
        assert_eq!(sanitize_title("   \n\t  "), "");
    }

    /// Sanitizing twice must equal sanitizing once, or a value that
    /// round-trips through the graph could drift.
    #[test]
    fn test_sanitize_title_is_idempotent() {
        for raw in [
            "\u{1b}]0;x\u{7}deploy",
            &"x".repeat(200),
            "  padded  ",
            "\u{202e}gnp.exe",
            "plain",
        ] {
            let once = sanitize_title(raw);
            assert_eq!(sanitize_title(&once), once, "not a fixed point: {raw:?}");
        }
    }

    /// The pane clamp counts characters, not bytes — a multi-byte title
    /// must not be cut mid-scalar, and must not exceed the border budget.
    #[test]
    fn test_sanitize_title_clamped_counts_multibyte_by_chars() {
        let out = sanitize_title_clamped(&"日".repeat(120));
        assert_eq!(out.chars().count(), MAX_TITLE_CHARS);
        assert_eq!(out, "日".repeat(MAX_TITLE_CHARS));
    }

    /// `sanitize_title` itself does NOT shorten. Length is a display
    /// policy, and windows need the full string intact because they use
    /// it as a lookup key (issue #104 adversarial review).
    #[test]
    fn test_sanitize_title_does_not_shorten() {
        let long = "x".repeat(500);
        assert_eq!(sanitize_title(&long), long);
        let hostile = format!("{}\u{1b}{}", "a".repeat(100), "b".repeat(100));
        assert_eq!(
            sanitize_title(&hostile),
            format!("{}{}", "a".repeat(100), "b".repeat(100))
        );
    }

    /// Hostile bytes are removed BEFORE the clamp, so an attacker cannot
    /// push payload past the 64-char window with filler that later
    /// disappears.
    #[test]
    fn test_sanitize_title_strips_before_clamping() {
        let raw = format!("{}\u{1b}]0;PWNED\u{7}", "\u{1b}".repeat(100));
        let out = sanitize_title(&raw);
        assert!(!out.chars().any(|c| c.is_control()), "{out:?}");
        assert_eq!(out, "]0;PWNED");
    }

    /// `Window::new` is the single construction site for windows; it
    /// sanitizes so no code path can mint a window with a hostile title.
    #[test]
    fn test_window_new_sanitizes_title() {
        let sid = SessionId::new();
        let pid = PaneId::new();
        let w = Window::new(sid, "\u{1b}]0;PWNED\u{7}deploy", pid);
        assert_eq!(w.title, "]0;PWNEDdeploy");
        assert!(!w.title.chars().any(|c| c.is_control()));
    }

    /// `Window::new` sanitizes but does NOT clamp — see
    /// `sanitize_title`. The graph rejects over-long window titles at
    /// ingress instead, so a lookup key is never silently truncated.
    #[test]
    fn test_window_new_does_not_clamp_long_title() {
        let sid = SessionId::new();
        let pid = PaneId::new();
        let w = Window::new(sid, "z".repeat(200), pid);
        assert_eq!(w.title.chars().count(), 200);
    }

    /// The bidi MARKS travel with the overrides — same class, same lint
    /// (`rustc`'s `text_direction_codepoint_in_literal`).
    #[test]
    fn test_sanitize_title_strips_bidi_marks() {
        for (label, ch) in [("LRM", '\u{200e}'), ("RLM", '\u{200f}'), ("ALM", '\u{61c}')] {
            assert_eq!(sanitize_title(&format!("a{ch}b")), "ab", "{label} survived");
        }
    }

    /// ZWJ is load-bearing inside emoji sequences — stripping it would
    /// split a legitimate title into separate glyphs.
    #[test]
    fn test_sanitize_title_keeps_zero_width_joiner() {
        let family = "build \u{1f468}\u{200d}\u{1f4bb} ok";
        assert_eq!(sanitize_title(family), family);
    }

    /// Auto-derived pane titles are as untrusted as the OSC and manual
    /// sources: a template picks both the argv and the cwd.
    #[test]
    fn test_pane_auto_title_from_command_is_sanitized() {
        let wid = WindowId::new();
        let pane = Pane::with_command(
            wid,
            "/home/test",
            vec!["sh\u{1b}]0;CMDPWN\u{7}".into(), "-c".into()],
        );
        assert_eq!(pane.title, "sh]0;CMDPWN");
        assert!(!pane.title.chars().any(|c| c.is_control()));
    }

    #[test]
    fn test_pane_auto_title_from_cwd_is_sanitized() {
        let wid = WindowId::new();
        let pane = Pane::new(wid, "/tmp/dir\u{1b}]0;CWDPWN\u{7}x");
        assert_eq!(pane.title, "dir]0;CWDPWNx");
        assert!(!pane.title.chars().any(|c| c.is_control()));
    }

    #[test]
    fn test_pane_title_serde_round_trips() {
        let wid = WindowId::new();
        let mut pane = Pane::with_command(wid, "/home/test", vec!["bash".into()]);
        pane.set_manual_title(Some("agent-1".into()));
        pane.set_osc_title("from-shell".into());
        let json = serde_json::to_string(&pane).unwrap();
        let back: Pane = serde_json::from_str(&json).unwrap();
        // After round-trip, recalculate_title runs implicitly in Deserialize?
        // No — Pane uses derive(Deserialize), so the fields come back as
        // stored. effective_title() still resolves correctly from the
        // stored fields.
        assert_eq!(back.manual_title.as_deref(), Some("agent-1"));
        assert_eq!(back.osc_title.as_deref(), Some("from-shell"));
        assert_eq!(back.effective_title(), "agent-1");
    }
}
