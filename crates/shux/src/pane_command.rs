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

/// Longest single argument the kernel will accept (`MAX_ARG_STRLEN` on Linux —
/// `PAGE_SIZE * 32`). Past this, `execve` fails with `E2BIG` and the pane never
/// spawns; caught here so the caller gets a parameter error naming the field.
const MAX_ARG_BYTES: usize = 128 * 1024;

/// The shell that interprets a string-form `command`.
///
/// Read from the **daemon's** environment, which is where every other shell
/// decision in shux is made — `PtyConfig::resolve_command` reads the same variable
/// for the default pane shell, so the two cannot drift.
pub(crate) fn interpreting_shell() -> String {
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
    if argv.first().is_some_and(|p| p.trim().is_empty()) {
        return Err(RpcError::invalid_params(&format!(
            "{what}[0] is blank — argv[0] must name a program to execute"
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

        // Exactly at the limit is fine.
        let ok = "x".repeat(MAX_ARG_BYTES);
        assert!(parse(json!(["echo", ok])).is_ok());
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
}
