//! The one place a `command` RPC parameter becomes the argv a pane execs.
//!
//! Five RPCs spawn a pane with a caller-supplied command — `session.create`,
//! `session.ensure`, `window.create`, `window.ensure`, `pane.split` — and before
//! issue #125 each parsed the field for itself. They disagreed in ways that were
//! invisible from the outside, because every disagreement ended in a pane that
//! started successfully and ran the wrong thing:
//!
//! - `session.*` split a string on whitespace, so `--cmd "printf 'X\n'; sleep 300"`
//!   exec'd `printf` with `'X\n';`, `sleep` and `300` as three arguments, while the
//!   flag's own help called it a *shell* command.
//! - `window.*` and `pane.split` matched `.as_array()` only, so a string was dropped
//!   on the floor and the pane got the default shell.
//! - All five used `filter_map(as_str)` over the array form, so `["vim", null]` ran
//!   `vim` with the null quietly deleted.
//! - Anything that was neither string nor array — a number, an object — vanished the
//!   same way.
//!
//! The contract implemented here, for all five:
//!
//! | `command` | meaning |
//! |---|---|
//! | `["nvim", "a b.rs"]` | argv — exec'd directly, no shell, no splitting |
//! | `"cargo build \|& tee log"` | a **shell** command — `$SHELL -c <string>` |
//! | absent / `null` / `""` / `[]` | the user's default login+interactive shell |
//! | anything else | [`RpcError::invalid_params`] naming what was wrong |
//!
//! The string form uses `$SHELL` because that is the shell a pane opened by hand
//! already runs (`PtyConfig::resolve_command` spawns `$SHELL -l -i`), so a `--cmd`
//! line gets the same *language* — bash syntax under bash, fish syntax under fish.
//!
//! It is `-c`, not `-l -c` or `-i -c`, which is what tmux's `new-session
//! <shell-command>` does and what `system(3)` does. That means **no startup files
//! are read**: a shell function or alias defined in `~/.bashrc`, and a `PATH` entry
//! added there, are not visible to a `--cmd` string. Interactive startup would drag
//! in job control, prompt setup and `$-`-conditional rc branches, none of which a
//! one-shot command wants. Callers that need their rc file can ask for it:
//! `-- bash -lic "…"`.

use shux_rpc::RpcError;

/// Fallback when `$SHELL` is unset or blank. POSIX guarantees this path.
const FALLBACK_SHELL: &str = "/bin/sh";

/// Longest single argument `execve` will accept.
///
/// Linux's `MAX_ARG_STRLEN` is `PAGE_SIZE * 32` = 131072, and it counts the
/// terminating NUL — so the largest string that actually fits is 131071. The
/// first cut of this used 131072 and let exactly that length through to fail at
/// `execve` with "Argument list too long", which is the outcome the check
/// exists to prevent. Bisected against the real binary, not reasoned about.
const MAX_ARG_BYTES: usize = 128 * 1024 - 1;

/// Ceiling on the whole argv — **shux's own limit, not a guess at the kernel's.**
///
/// The distinction matters, because a cap that claims to predict `execve` is
/// wrong in both directions and was removed for exactly that reason: `ARG_MAX`
/// is shared with the environment, so a 1.2 MB environment still produced
/// `E2BIG` under a 1 MB argv that passed; and an ordinary environment exec'd
/// 1.5 MB that the same cap refused. Whether `execve` will accept an argv is
/// the kernel's call, and it is diagnosed at spawn (`spawn_failure` in
/// `main.rs` names `ARG_MAX` when that is what happened).
///
/// This bounds something shux genuinely owns: an argv is **persisted on the
/// pane, cloned into every graph snapshot, and echoed in full by `pane.list`
/// and in `PaneCreated`**. `state.apply` keeps a pane whose spawn failed, so a
/// few multi-megabyte argvs pushed `pane.list` past the 16 MB frame limit and
/// every read of that session died with `early eof` — recoverable only by
/// killing it. 256 KiB is two orders of magnitude past any real command line
/// and two below the frame limit.
const MAX_ARGV_BYTES: usize = 256 * 1024;

/// The shell that interprets a string-form `command`.
///
/// Whatever a pane opened by hand runs, a `--cmd` string is interpreted by —
/// otherwise the same line means bash syntax in one place and fish syntax in
/// the other. So the resolution order mirrors the default pane shell exactly:
/// `[shell].command`'s program when the user configured one (issue #132),
/// otherwise the **daemon's** `$SHELL`, otherwise `/bin/sh`.
///
/// Only the program is taken from `[shell].command`, never its flags: those
/// are login/interactive flags for a shell you sit in (`-l -i`), and this
/// invocation is `-c` — see the module header on why one-shot commands do not
/// read startup files.
pub(crate) fn interpreting_shell() -> String {
    if let Some(argv) = crate::configured_shell_argv(&crate::daemon_shell_config()) {
        // `configured_shell_argv` already rejected a blank program, so the
        // first element is a real name. Same predicate the pane shell uses —
        // the two cannot disagree about whether a shell is configured.
        return argv[0].clone();
    }
    std::env::var("SHELL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| FALLBACK_SHELL.to_string())
}

/// Parse the `command` member of a pane-spawning RPC's params.
///
/// An empty vector means "spawn the user's default shell" — the same signal the
/// PTY layer has always used.
pub(crate) fn parse_pane_command(params: &serde_json::Value) -> Result<Vec<String>, RpcError> {
    parse_pane_command_with_shell(params.get("command"), &interpreting_shell())
}

/// [`parse_pane_command`] with the shell injected, so the contract can be tested
/// without touching process-wide environment.
pub(crate) fn parse_pane_command_with_shell(
    command: Option<&serde_json::Value>,
    shell: &str,
) -> Result<Vec<String>, RpcError> {
    use serde_json::Value;

    match command {
        None | Some(Value::Null) => Ok(Vec::new()),

        // Shell command. Blank (or whitespace-only) means "no command given" —
        // `--cmd ""` should open a shell, not run one that does nothing.
        Some(Value::String(s)) => {
            if s.trim().is_empty() {
                return Ok(Vec::new());
            }
            reject_unexecutable(s, "'command'")?;
            Ok(vec![shell.to_string(), "-c".to_string(), s.clone()])
        }

        // argv passthrough. Every element must really be a string: dropping the
        // ones that are not is how `["vim", null]` used to become a bare `vim`.
        Some(Value::Array(items)) => {
            let mut argv = Vec::with_capacity(items.len());
            for (i, item) in items.iter().enumerate() {
                let Some(s) = item.as_str() else {
                    return Err(RpcError::invalid_params(&format!(
                        "'command[{i}]' is {}, but every element of an argv array must be a \
                         string — pass a single string instead to run it through the shell",
                        describe(item)
                    )));
                };
                reject_unexecutable(s, &format!("'command[{i}]'"))?;
                argv.push(s.to_string());
            }
            reject_oversize_argv(&argv, "'command'")?;
            // `[""]` and `["   "]` both exec a program name that cannot resolve:
            // the pane dies instantly with an error naming neither the pane nor
            // the cause. Blank is checked with `trim`, matching how the string
            // form treats a blank command.
            if argv.first().is_some_and(|p| p.trim().is_empty()) {
                return Err(RpcError::invalid_params(
                    "'command[0]' is blank — argv[0] must name a program to execute",
                ));
            }
            Ok(argv)
        }

        Some(other) => Err(RpcError::invalid_params(&format!(
            "'command' is {}, but must be a string (a shell command, e.g. \"npm run dev\") \
             or an array of strings (argv, e.g. [\"npm\", \"run\", \"dev\"])",
            describe(other)
        ))),
    }
}

/// Parse `pane.run_command`'s `args` — an argument list, not an argv.
///
/// `args` is a different shape from `command`: it never carries the program
/// name (that is the sibling `command` string, which is a shell line), so
/// `args[0]` is an ordinary argument and `""` is a legal one. Everything else
/// is the same contract, and it was being read with `filter_map(as_str)` — the
/// exact silent-drop issue #125 removed from the five spawning RPCs, still live
/// on the sixth: `["a", null, "b"]` ran `a b` and reported success.
///
/// It also validates for a **different sink**. Every other command in this
/// module ends at `execve`, and [`reject_unexecutable`] is written for that:
/// the one byte `execve` cannot carry is NUL. `args` never reaches `execve` —
/// `CommandEngine::start_command` quotes it into a line and **types that line
/// into the pane's terminal**, where the tty line discipline reads it first and
/// NUL is the byte it harmlessly discards. See [`reject_untypeable`].
pub(crate) fn parse_run_args(params: &serde_json::Value) -> Result<Vec<String>, RpcError> {
    use serde_json::Value;

    match params.get("args") {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => {
            let mut args = Vec::with_capacity(items.len());
            for (i, item) in items.iter().enumerate() {
                let Some(s) = item.as_str() else {
                    return Err(RpcError::invalid_params(&format!(
                        "'args[{i}]' is {}, but every element of an argument list must be a \
                         string — put shell text in 'command' instead",
                        describe(item)
                    )));
                };
                reject_unexecutable(s, &format!("'args[{i}]'"))?;
                reject_untypeable(s, &format!("'args[{i}]'"))?;
                args.push(s.to_string());
            }
            reject_oversize_argv(&args, "'args'")?;
            Ok(args)
        }
        Some(other) => Err(RpcError::invalid_params(&format!(
            "'args' is {}, but must be an array of strings",
            describe(other)
        ))),
    }
}

/// Validate an argv that did NOT come through [`parse_pane_command`].
///
/// `state.apply` takes its ops as typed structs, so serde already guarantees
/// `Vec<String>` and the JSON-shape errors above cannot arise. What serde does
/// not check is whether the strings can actually reach `execve`: `[""]` and a
/// NUL-bearing argument both commit a session, a window and a pane whose PTY
/// then fails to spawn — the exact outcomes this module exists to turn into
/// parameter errors (issue #125 follow-up).
pub(crate) fn validate_argv(argv: &[String], what: &str) -> Result<(), RpcError> {
    for (i, arg) in argv.iter().enumerate() {
        reject_unexecutable(arg, &format!("{what}[{i}]"))?;
    }
    reject_oversize_argv(argv, what)?;
    if argv.first().is_some_and(|p| p.trim().is_empty()) {
        return Err(RpcError::invalid_params(&format!(
            "{what}[0] is blank — argv[0] must name a program to execute"
        )));
    }
    Ok(())
}

/// Validate every argv carried by a lowered `state.apply` batch.
///
/// Shared by the daemon's `state.apply` handler and by the CLI's `--dry-run`,
/// which is the whole point: dry-run exists to answer "will this apply
/// succeed?", and it answered yes to templates the real run rejects because the
/// only copy of the rule lived server-side (issue #125 follow-up).
pub(crate) fn validate_ops(ops: &[shux_core::apply::Op]) -> Result<(), RpcError> {
    use shux_core::apply::Op;
    for (i, op) in ops.iter().enumerate() {
        let (argv, field) = match op {
            Op::CreateSession {
                initial_command, ..
            }
            | Op::CreateWindow {
                initial_command, ..
            } => (initial_command, "initial_command"),
            Op::SplitPane { command, .. } => (command, "command"),
        };
        validate_argv(argv, &format!("ops[{i}].{field}"))?;

        // Same reasoning as the titles below: the ratio's range lived only in
        // the CLI flag's help text, and the layout engine clamps rather than
        // refuses — so `ratio = 5.0` in a template applied successfully and
        // produced an unusable sliver pane (issue #136). Validating it here
        // covers `state.apply`, `session restore` and both `--dry-run`s at
        // once, since they all funnel through this function.
        if let Op::SplitPane { ratio, .. } = op
            && (!ratio.is_finite() || *ratio <= 0.0 || *ratio >= 1.0)
        {
            return Err(RpcError::invalid_params(&format!(
                "ops[{i}].ratio: {ratio} is out of range: must be above 0.0 and below 1.0"
            )));
        }

        // Window titles are a pure rule too, and lived only behind a graph
        // mutation — so a 300-character title, or one that sanitizes to
        // nothing, passed `--dry-run` and failed the apply.
        if let Op::CreateWindow { title, .. } = op
            && let Err(e) = shux_core::graph::SessionGraph::check_window_title(title)
        {
            return Err(RpcError::invalid_params(&format!("ops[{i}].title: {e}")));
        }
        // `Some("")` is not a title — it is the absence of one. `title` is a
        // required TOML field, so a template with nothing to say writes `""`,
        // and `stage_create_session` substitutes the default `"1"` for it.
        // Validating it here would reject templates the real apply accepts,
        // which is the same dry-run/apply divergence in the other direction.
        if let Op::CreateSession {
            initial_window_title: Some(title),
            ..
        } = op
            && !title.is_empty()
            && let Err(e) = shux_core::graph::SessionGraph::check_window_title(title)
        {
            return Err(RpcError::invalid_params(&format!(
                "ops[{i}].initial_window_title: {e}"
            )));
        }
    }
    Ok(())
}

/// Reject a string that cannot survive being **typed into a terminal**.
///
/// `pane.run_command` does not exec anything. It shell-quotes its `args`, joins
/// them into a line with the completion marker on the end, and writes that line
/// to the pane's PTY — so the first thing to read it is the tty line
/// discipline, not a shell. In canonical mode that layer *consumes* control
/// bytes on sight: `0x03` is INTR, `0x15` kills the line, `0x1a` is SUSP,
/// `0x7f` erases, `0x04` is EOF, and `\n`/`\r` submit the line early. Which
/// byte does what is `termios`-configurable, so the set cannot be enumerated
/// with any confidence — the whole class goes.
///
/// This is not theoretical tidiness. Quoting an argument correctly (which the
/// allowlist in `shux_pty::shell_quote_arg` now does) puts the control byte
/// **inside single quotes**, so when the line discipline eats it the line is
/// truncated mid-quote and the shell drops to its continuation prompt and stays
/// there — swallowing every later command sent to that pane. One `0x03` in one
/// argument wedged the pane permanently. The looser quoting this task replaced
/// happened to leave the truncated remainder syntactically valid, so the same
/// input merely failed once; making the quoting correct turned a transient
/// error into a permanent one, and the missing validation is what made that
/// possible.
///
/// Rejecting is right rather than sanitizing: an argument that cannot be
/// delivered verbatim is one the caller must be told about, not one to deliver
/// approximately (issue #125's rule for this whole family).
fn reject_untypeable(s: &str, what: &str) -> Result<(), RpcError> {
    if let Some(c) = s.chars().find(|c| (*c as u32) < 0x20 || *c == '\u{7f}') {
        return Err(RpcError::invalid_params(&format!(
            "{what} contains the control character U+{:04X}, which cannot be typed into a \
             terminal — the tty line discipline acts on it before any shell sees it. Put shell \
             text in 'command' instead.",
            c as u32
        )));
    }
    Ok(())
}

/// Reject the two strings `execve` cannot carry, so the caller gets a parameter
/// error instead of a pane that silently fails to spawn.
fn reject_unexecutable(s: &str, what: &str) -> Result<(), RpcError> {
    if s.contains('\0') {
        return Err(RpcError::invalid_params(&format!(
            "{what} contains a NUL byte, which cannot be passed to a program"
        )));
    }
    if s.len() > MAX_ARG_BYTES {
        return Err(RpcError::invalid_params(&format!(
            "{what} is {} bytes; a single argument cannot exceed {MAX_ARG_BYTES}",
            s.len()
        )));
    }
    Ok(())
}

/// Reject an argv larger than shux will carry. See [`MAX_ARGV_BYTES`] — this is
/// a bound on what the graph stores and echoes, not a prediction about `execve`.
fn reject_oversize_argv(argv: &[String], what: &str) -> Result<(), RpcError> {
    // +1 per element for the NUL that travels with each argument.
    let total: usize = argv.iter().map(|a| a.len() + 1).sum();
    if total > MAX_ARGV_BYTES {
        return Err(RpcError::invalid_params(&format!(
            "{what} is {total} bytes across {} arguments; shux stores and reports \
             a pane's command in full, so the whole argv cannot exceed {MAX_ARGV_BYTES}",
            argv.len()
        )));
    }
    Ok(())
}

/// Name a JSON value's type for an error message, the way a human would.
fn describe(v: &serde_json::Value) -> &'static str {
    use serde_json::Value;
    match v {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(v: serde_json::Value) -> Result<Vec<String>, RpcError> {
        parse_pane_command_with_shell(Some(&v), "/bin/bash")
    }

    // ── argv form ───────────────────────────────────────────────────────

    #[test]
    fn argv_array_is_passed_through_untouched() {
        assert_eq!(
            parse(json!(["nvim", "src/a b.rs"])).unwrap(),
            vec!["nvim".to_string(), "src/a b.rs".to_string()]
        );
    }

    #[test]
    fn argv_preserves_shell_metacharacters_as_literal_arguments() {
        // The whole point of the argv form: nothing interprets these.
        assert_eq!(
            parse(json!(["echo", "a;b|c", "$HOME", "*"])).unwrap(),
            vec!["echo", "a;b|c", "$HOME", "*"]
        );
    }

    #[test]
    fn empty_array_means_default_shell() {
        assert_eq!(parse(json!([])).unwrap(), Vec::<String>::new());
    }

    // ── shell-string form (issue #125) ──────────────────────────────────

    #[test]
    fn string_becomes_a_shell_invocation_not_a_whitespace_split() {
        assert_eq!(
            parse(json!("printf 'X\n'; sleep 300")).unwrap(),
            vec!["/bin/bash", "-c", "printf 'X\n'; sleep 300"]
        );
    }

    #[test]
    fn string_with_a_pipe_stays_one_shell_command() {
        assert_eq!(
            parse(json!("ls -1 | wc -l")).unwrap(),
            vec!["/bin/bash", "-c", "ls -1 | wc -l"]
        );
    }

    #[test]
    fn simple_string_still_runs_the_same_program() {
        // The overwhelmingly common case must not change meaning.
        assert_eq!(parse(json!("top")).unwrap(), vec!["/bin/bash", "-c", "top"]);
    }

    #[test]
    fn blank_string_means_default_shell() {
        assert_eq!(parse(json!("")).unwrap(), Vec::<String>::new());
        assert_eq!(parse(json!("   \t ")).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn missing_or_null_means_default_shell() {
        assert_eq!(
            parse_pane_command_with_shell(None, "/bin/bash").unwrap(),
            Vec::<String>::new()
        );
        assert_eq!(parse(json!(null)).unwrap(), Vec::<String>::new());
    }

    // ── rejections ──────────────────────────────────────────────────────

    #[test]
    fn a_number_is_rejected_not_ignored() {
        let err = parse(json!(42)).unwrap_err();
        let msg = format!("{err:?}");
        assert!(msg.contains("a number"), "{msg}");
        assert!(msg.contains("argv"), "{msg}");
    }

    #[test]
    fn a_bool_is_rejected() {
        assert!(parse(json!(true)).is_err());
    }

    #[test]
    fn an_object_is_rejected() {
        let err = parse(json!({"argv": ["vim"]})).unwrap_err();
        assert!(format!("{err:?}").contains("an object"));
    }

    #[test]
    fn a_non_string_argv_element_is_rejected_not_dropped() {
        let err = parse(json!(["vim", null])).unwrap_err();
        let msg = format!("{err:?}");
        assert!(msg.contains("command[1]"), "{msg}");
        assert!(msg.contains("null"), "{msg}");
    }

    #[test]
    fn a_nested_array_element_is_rejected() {
        let err = parse(json!(["sh", ["-c", "x"]])).unwrap_err();
        assert!(format!("{err:?}").contains("command[1]"));
    }

    #[test]
    fn a_blank_program_name_is_rejected() {
        for blank in [json!([""]), json!(["   "]), json!(["\t"])] {
            let err = parse(blank.clone()).unwrap_err();
            assert!(
                format!("{err:?}").contains("command[0]"),
                "{blank} was accepted"
            );
        }
    }

    #[test]
    fn an_argument_too_long_for_execve_is_rejected() {
        let huge = "x".repeat(MAX_ARG_BYTES + 1);
        let err = parse(json!(huge)).unwrap_err();
        assert!(format!("{err:?}").contains("cannot exceed"), "{err:?}");

        let err = parse(json!(["echo", huge])).unwrap_err();
        let msg = format!("{err:?}");
        assert!(msg.contains("command[1]"), "{msg}");

        // The boundary is the point. `MAX_ARG_STRLEN` counts the terminating
        // NUL, so 131071 is the longest argument that can actually be exec'd and
        // 131072 is not — the first cut of this had the cap one byte too high
        // and asserted, greenly, that the byte length which fails at `execve`
        // was fine. `crates/shux/tests/pane_command_e2e.rs` pins the same
        // boundary against a real spawn.
        assert_eq!(MAX_ARG_BYTES, 131071);
        assert!(parse(json!(["echo", "x".repeat(MAX_ARG_BYTES)])).is_ok());
        assert!(parse(json!(["echo", "x".repeat(MAX_ARG_BYTES + 1)])).is_err());
    }

    /// The aggregate bound is about what shux stores and echoes, so the message
    /// says that rather than claiming to know what `execve` would have done.
    #[test]
    fn an_argv_larger_than_shux_will_carry_is_rejected() {
        let argv: Vec<_> = std::iter::once("echo".to_string())
            .chain(std::iter::repeat_n("x".repeat(100_000), 40))
            .collect();
        let err = parse(serde_json::Value::Array(
            argv.into_iter().map(serde_json::Value::String).collect(),
        ))
        .unwrap_err();
        let msg = format!("{err:?}");
        assert!(msg.contains("stores and reports"), "{msg}");

        // An ordinary command line — even a generous one — is nowhere near it.
        let ok: Vec<_> = std::iter::once("grep".to_string())
            .chain(std::iter::repeat_n(
                "some/path/to/a/file.rs".to_string(),
                2000,
            ))
            .collect();
        assert!(
            parse(serde_json::Value::Array(
                ok.into_iter().map(serde_json::Value::String).collect()
            ))
            .is_ok()
        );
    }

    #[test]
    fn a_later_empty_argument_is_allowed() {
        // `find . -name ''` is legitimate; only argv[0] must name a program.
        assert_eq!(parse(json!(["find", ""])).unwrap(), vec!["find", ""]);
    }

    #[test]
    fn a_nul_in_a_shell_string_is_rejected() {
        let err = parse(json!("echo \u{0}hi")).unwrap_err();
        assert!(format!("{err:?}").contains("NUL"));
    }

    /// `pane.run_command`'s `args` are TYPED INTO A TERMINAL, not exec'd. The
    /// line discipline acts on control bytes before any shell reads them, and
    /// because a correctly quoted argument puts the byte inside single quotes,
    /// the truncated line leaves the shell at its continuation prompt — where
    /// it swallows every later command sent to that pane. Reproduced against
    /// the real binary: one `0x03` wedged the pane permanently.
    #[test]
    fn a_control_character_in_run_args_is_rejected() {
        for (c, name) in [
            ('\u{3}', "INTR"),
            ('\u{15}', "KILL"),
            ('\u{1a}', "SUSP"),
            ('\u{7f}', "DEL"),
            ('\u{4}', "EOF"),
            ('\n', "newline"),
            ('\r', "CR"),
            ('\t', "TAB"),
            ('\u{1b}', "ESC"),
        ] {
            let params = serde_json::json!({ "args": ["ok", format!("a{c}b")] });
            let err =
                parse_run_args(&params).expect_err(&format!("{name} ({c:?}) must be rejected"));
            let msg = format!("{err:?}");
            assert!(
                msg.contains("args[1]") && msg.contains(&format!("U+{:04X}", c as u32)),
                "{name}: error must name the element and the byte, got {msg}"
            );
        }
    }

    /// The ordinary shapes must still pass — including the empty string, which
    /// is a legal ARGUMENT even though it is not a legal argv[0].
    #[test]
    fn run_args_accepts_every_printable_shape() {
        let params = serde_json::json!({
            "args": ["", "a b", "a;id", "--flag=value", "=eq", "café", "中文", "it's"]
        });
        let args = parse_run_args(&params).expect("printable args are fine");
        assert_eq!(args.len(), 8);
        assert_eq!(args[0], "");
    }

    #[test]
    fn a_nul_in_an_argv_element_is_rejected() {
        let err = parse(json!(["echo", "a\u{0}b"])).unwrap_err();
        let msg = format!("{err:?}");
        assert!(msg.contains("NUL"), "{msg}");
        assert!(msg.contains("command[1]"), "{msg}");
    }

    // ── shell resolution ────────────────────────────────────────────────

    #[test]
    fn shell_falls_back_when_unset_or_blank() {
        // `interpreting_shell` reads the process environment; assert the policy it
        // encodes without mutating global state in a test.
        let resolved = |v: Option<&str>| -> String {
            v.map(str::to_string)
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| FALLBACK_SHELL.to_string())
        };
        assert_eq!(resolved(None), "/bin/sh");
        assert_eq!(resolved(Some("")), "/bin/sh");
        assert_eq!(resolved(Some("  ")), "/bin/sh");
        assert_eq!(resolved(Some("/usr/bin/zsh")), "/usr/bin/zsh");
    }

    #[test]
    fn interpreting_shell_is_absolute_or_a_name_never_empty() {
        assert!(!interpreting_shell().trim().is_empty());
    }

    // ── validate_argv, the `state.apply` entry point ────────────────────

    #[test]
    fn validate_argv_rejects_what_execve_cannot_carry() {
        assert!(validate_argv(&[], "op[0].command").is_ok());
        assert!(validate_argv(&["vim".into()], "op[0].command").is_ok());

        let err = validate_argv(&["".into()], "op[0].command").unwrap_err();
        assert!(format!("{err:?}").contains("op[0].command[0]"), "{err:?}");

        let err = validate_argv(&["   ".into()], "op[0].command").unwrap_err();
        assert!(format!("{err:?}").contains("blank"), "{err:?}");

        let err = validate_argv(&["echo".into(), "a\u{0}b".into()], "c").unwrap_err();
        let msg = format!("{err:?}");
        assert!(msg.contains("NUL") && msg.contains("c[1]"), "{msg}");
    }

    #[test]
    fn parse_reads_the_command_member_of_params() {
        let params = json!({"name": "x", "command": ["vim"]});
        assert_eq!(parse_pane_command(&params).unwrap(), vec!["vim"]);
        let params = json!({"name": "x"});
        assert_eq!(parse_pane_command(&params).unwrap(), Vec::<String>::new());
    }

    // ── validate_ops must agree with the graph, in both directions ──────

    fn create_session_titled(title: Option<&str>) -> shux_core::apply::Op {
        shux_core::apply::Op::CreateSession {
            name: Some("s".into()),
            cwd: std::path::PathBuf::from("/tmp"),
            initial_command: Vec::new(),
            initial_window_title: title.map(str::to_string),
        }
    }

    #[test]
    fn blank_initial_window_title_is_unspecified_not_invalid() {
        // `title` is a required TOML field, so a template with nothing to say
        // writes `""`. `stage_create_session` substitutes `"1"` for it, and
        // `state apply` has always accepted that — so the pre-flight check
        // must not be stricter than the apply it is predicting.
        validate_ops(&[create_session_titled(Some(""))]).expect("`\"\"` means unspecified");
        validate_ops(&[create_session_titled(None)]).expect("`None` means unspecified");
    }

    #[test]
    fn a_title_that_sanitizes_to_nothing_is_still_rejected() {
        // The attack case is different from the blank case: content went in,
        // nothing came out. That must not reach the graph.
        let err = validate_ops(&[create_session_titled(Some("\u{200b}\u{200b}"))]).unwrap_err();
        assert!(
            format!("{err:?}").contains("ops[0].initial_window_title"),
            "{err:?}"
        );

        let long = "w".repeat(500);
        let err = validate_ops(&[create_session_titled(Some(&long))]).unwrap_err();
        assert!(
            format!("{err:?}").contains("ops[0].initial_window_title"),
            "{err:?}"
        );
    }

    #[test]
    fn create_window_still_rejects_a_blank_title() {
        // `CreateWindow` has no "unspecified" spelling — `stage_create_window`
        // validates unconditionally, so this pre-flight check must too.
        let op = shux_core::apply::Op::CreateWindow {
            session: shux_core::apply::SessionRef::BackRef { op_index: 0 },
            title: String::new(),
            cwd: None,
            initial_command: Vec::new(),
        };
        let err = validate_ops(&[op]).unwrap_err();
        assert!(format!("{err:?}").contains("ops[0].title"), "{err:?}");
    }
}
