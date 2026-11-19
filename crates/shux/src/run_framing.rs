//! Whether `pane.run_command` may type a line into a pane now, and how it
//! finds the line's command: the shell's own OSC 133 marks, or printed
//! markers. A line goes only to a shell's empty prompt, unless the caller
//! asked for markers; every other state waits for a prompt or is refused by
//! name.

use shux_pty::Boundaries;
use shux_vt::ShellState;

/// `boundaries` as the caller asked for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Wanted {
    /// The shell's own marks when it reports them, else markers.
    Auto,
    Osc133,
    /// Markers, typed into whatever holds the pane.
    Markers,
}

/// What reads the pane's input now.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Front {
    pub(crate) shell: ShellState,
    /// A line submitted at the prompt has not started, or input is still
    /// queued for the pane.
    pub(crate) input_pending: bool,
    /// Keys were sent to the prompt that submitted nothing.
    pub(crate) input_present: bool,
    /// The cursor is at the start of a row. A drawn prompt never leaves it
    /// there; a line editor that has accepted a line, or prompt hooks still
    /// running after the prompt mark, do.
    pub(crate) at_line_start: bool,
    /// `None` when the terminal could not be asked.
    pub(crate) program: Option<shux_pty::Foreground>,
    pub(crate) full_screen: bool,
    /// The pane's own program is a shell.
    pub(crate) pane_is_shell: bool,
}

/// Why nothing may be typed into the pane now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refusal {
    HiddenInput,
    OtherProgram,
    FullScreen,
    CommandRunning,
    InputPending,
    InputPresent,
    NotAShell,
    /// Nothing reported a prompt: `osc133` was asked for, or a command ended
    /// and its prompt never came.
    NoPrompt,
}

impl Refusal {
    pub(crate) fn reason(self) -> &'static str {
        match self {
            Refusal::HiddenInput => "hidden_input",
            Refusal::OtherProgram => "other_program",
            Refusal::FullScreen => "full_screen",
            Refusal::CommandRunning => "command_running",
            Refusal::InputPending => "input_pending",
            Refusal::InputPresent => "input_present",
            Refusal::NotAShell => "not_a_shell",
            Refusal::NoPrompt => "no_prompt",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Choice {
    Use(Boundaries),
    /// Type nothing yet; at the deadline, refuse with this.
    Await(Refusal),
    Refuse(Refusal),
    /// The request cannot be met in this pane at all.
    Invalid(&'static str),
}

/// The framing for the pane as it is now.
pub(crate) fn choose(wanted: Wanted, front: &Front, multiline: bool) -> Choice {
    use ShellState::{AtPrompt, Between, Running, Unknown};
    if wanted == Wanted::Markers {
        return Choice::Use(Boundaries::Markers);
    }
    if multiline && (wanted == Wanted::Osc133 || front.shell != Unknown) {
        return Choice::Invalid(
            "a shell that reports its boundaries runs a command per line: join the lines with `;` or `&&`, or pass boundaries \"markers\"",
        );
    }
    if let Some(refusal) = refusal(front) {
        // A prompt hook runs its programs after the prompt mark: wait them out.
        let hook = front.shell == AtPrompt && refusal == Refusal::OtherProgram;
        return if hook {
            Choice::Await(refusal)
        } else {
            Choice::Refuse(refusal)
        };
    }
    match (wanted, front.shell) {
        (_, AtPrompt) if front.at_line_start => Choice::Await(Refusal::InputPending),
        (_, AtPrompt) => Choice::Use(Boundaries::Osc133),
        (Wanted::Auto, Unknown) if front.pane_is_shell => Choice::Use(Boundaries::Markers),
        (Wanted::Auto, Unknown) => Choice::Refuse(Refusal::NotAShell),
        (_, Unknown | Between) => Choice::Await(Refusal::NoPrompt),
        (_, Running) => Choice::Refuse(Refusal::CommandRunning),
    }
}

/// The first thing, most specific first, that would read a typed line other
/// than the shell's empty prompt.
fn refusal(front: &Front) -> Option<Refusal> {
    let program = front.program;
    if program.is_some_and(|p| p.hidden_input) {
        Some(Refusal::HiddenInput)
    } else if program.is_some_and(|p| !p.leader) {
        Some(Refusal::OtherProgram)
    } else if front.full_screen {
        Some(Refusal::FullScreen)
    } else if front.shell == ShellState::Running {
        Some(Refusal::CommandRunning)
    } else if front.input_pending {
        Some(Refusal::InputPending)
    } else if front.input_present {
        Some(Refusal::InputPresent)
    } else {
        None
    }
}

/// Whether a pane's argv runs a shell: empty runs the user's login shell.
pub(crate) fn runs_a_shell(argv: &[String]) -> bool {
    const SHELLS: &[&str] = &[
        "sh", "bash", "zsh", "dash", "ksh", "mksh", "oksh", "yash", "fish", "tcsh", "csh", "nu",
        "elvish", "xonsh", "pwsh",
    ];
    let Some(first) = argv.first() else {
        return true;
    };
    let name = std::path::Path::new(first)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    // `-c` runs a script, not a prompt.
    SHELLS.contains(&name.trim_start_matches('-')) && !argv.iter().any(|a| a == "-c")
}

#[cfg(test)]
mod tests {
    use super::*;
    use Boundaries::{Markers, Osc133};
    use ShellState::{AtPrompt, Between, Running, Unknown};

    fn front(shell: ShellState) -> Front {
        Front {
            shell,
            input_pending: false,
            input_present: false,
            at_line_start: false,
            program: Some(shux_pty::Foreground {
                leader: true,
                hidden_input: false,
                group: 1,
            }),
            full_screen: false,
            pane_is_shell: true,
        }
    }

    fn fg(leader: bool, hidden_input: bool) -> Option<shux_pty::Foreground> {
        Some(shux_pty::Foreground {
            leader,
            hidden_input,
            group: 9,
        })
    }

    /// The decision table: each framing asked for, in each state the shell
    /// can report.
    #[test]
    fn each_framing_in_each_shell_state() {
        use Choice::{Await, Refuse, Use};
        let rows = [
            (Wanted::Auto, Unknown, Use(Markers)),
            (Wanted::Auto, AtPrompt, Use(Osc133)),
            (Wanted::Auto, Between, Await(Refusal::NoPrompt)),
            (Wanted::Auto, Running, Refuse(Refusal::CommandRunning)),
            (Wanted::Osc133, Unknown, Await(Refusal::NoPrompt)),
            (Wanted::Osc133, AtPrompt, Use(Osc133)),
            (Wanted::Osc133, Between, Await(Refusal::NoPrompt)),
            (Wanted::Osc133, Running, Refuse(Refusal::CommandRunning)),
            (Wanted::Markers, Unknown, Use(Markers)),
            (Wanted::Markers, AtPrompt, Use(Markers)),
            (Wanted::Markers, Between, Use(Markers)),
            (Wanted::Markers, Running, Use(Markers)),
        ];
        for (wanted, shell, want) in rows {
            assert_eq!(
                choose(wanted, &front(shell), false),
                want,
                "{wanted:?} {shell:?}"
            );
        }
    }

    /// Whatever the marks say, input that would not reach an empty prompt is
    /// refused by name, most specific first; `markers` types anyway.
    #[test]
    fn a_pane_not_at_an_empty_prompt_is_refused_by_name() {
        let with = |f: fn(&mut Front)| {
            let mut front = front(AtPrompt);
            f(&mut front);
            choose(Wanted::Auto, &front, false)
        };

        assert_eq!(
            with(|f| f.program = fg(true, true)),
            Choice::Refuse(Refusal::HiddenInput)
        );
        assert_eq!(
            with(|f| f.program = fg(false, true)),
            Choice::Refuse(Refusal::HiddenInput)
        );
        assert_eq!(
            with(|f| f.full_screen = true),
            Choice::Refuse(Refusal::FullScreen)
        );
        assert_eq!(
            with(|f| f.input_pending = true),
            Choice::Refuse(Refusal::InputPending)
        );
        assert_eq!(
            with(|f| f.input_present = true),
            Choice::Refuse(Refusal::InputPresent)
        );
        // A line accepted and not started, or a prompt not drawn yet: wait.
        assert_eq!(
            with(|f| f.at_line_start = true),
            Choice::Await(Refusal::InputPending)
        );
        // At the prompt mark a prompt hook may still be running: wait.
        assert_eq!(
            with(|f| f.program = fg(false, false)),
            Choice::Await(Refusal::OtherProgram)
        );
        let mut running = front(Running);
        running.program = fg(false, false);
        assert_eq!(
            choose(Wanted::Auto, &running, false),
            Choice::Refuse(Refusal::OtherProgram)
        );
        // A terminal that cannot be asked does not block the marks' answer.
        assert_eq!(with(|f| f.program = None), Choice::Use(Osc133));
        let mut all = front(Running);
        all.program = fg(false, true);
        all.full_screen = true;
        assert_eq!(choose(Wanted::Markers, &all, true), Choice::Use(Markers));
    }

    /// A pane that reports nothing gets markers only if it runs a shell.
    #[test]
    fn markers_go_only_to_a_shell_that_reports_nothing() {
        let mut python = front(Unknown);
        python.pane_is_shell = false;
        assert_eq!(
            choose(Wanted::Auto, &python, false),
            Choice::Refuse(Refusal::NotAShell)
        );
        assert!(runs_a_shell(&[]));
        assert!(runs_a_shell(&["/bin/bash".into(), "-l".into()]));
        assert!(runs_a_shell(&["-zsh".into()]));
        assert!(!runs_a_shell(&["python3".into()]));
        assert!(!runs_a_shell(&["bash".into(), "-c".into(), "make".into()]));
    }

    /// Several lines are several commands to a shell that reports them.
    #[test]
    fn a_multi_line_command_runs_only_with_markers() {
        assert!(matches!(
            choose(Wanted::Auto, &front(AtPrompt), true),
            Choice::Invalid(_)
        ));
        assert!(matches!(
            choose(Wanted::Osc133, &front(Unknown), true),
            Choice::Invalid(_)
        ));
        assert_eq!(
            choose(Wanted::Auto, &front(Unknown), true),
            Choice::Use(Markers)
        );
    }
}
