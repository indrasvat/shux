//! Entry point, and nothing else.
//!
//! Every module lives in `lib.rs`. Declaring one here as well would compile it
//! into both targets as two unrelated types with the same name — see the note
//! on the crate docs in `lib.rs`. `scripts/check-no-bin-mods.sh` asserts this
//! file stays free of `mod` declarations.

use clap::{CommandFactory, FromArgMatches};

use shux::cli::{self, Cli, Command};
use shux::daemon_boot::run_daemon;
use shux::dispatch::run_client;
use shux::style;

fn main() {
    // Inject the colorised agent reference at runtime so it honours
    // NO_COLOR + the IsTerminal piped-stdout check. clap's derive macro
    // only accepts a `&'static str` literal there, so we set it here.
    let cmd = Cli::command()
        .before_help(style::banner())
        .long_about(cli::long_about())
        .after_long_help(cli::agent_help());
    let matches = cmd.try_get_matches().unwrap_or_else(|e| exit_from_clap(&e));
    let args = Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());

    let result = if matches!(args.command, Some(Command::__daemon)) {
        // Internal daemon subcommand — called by auto-start. `--socket` is a
        // global arg, so the re-exec'd daemon sees whatever the client saw.
        run_daemon(args.socket.clone())
    } else {
        // Normal CLI client mode
        end_on_a_failed_print();
        run_client(args).and_then(|()| {
            // A print without a final newline is still buffered here, and
            // std's flush at exit ignores its error. A stream whose reader
            // left has nothing more to deliver.
            use std::io::Write as _;
            if style::stream_reader_gone() {
                return Ok(());
            }
            std::io::stdout()
                .flush()
                .map_err(|e| style::OutputError(e).into())
        })
    };

    if let Err(e) = result {
        report_fatal(&e);
    }
}

/// A print that fails ends the command: a closed stdout or stderr the Unix
/// way, killed by SIGPIPE with no panic text; any other failure with its cause
/// on stderr and exit 1. Only those prints: SIGPIPE stays ignored otherwise, so
/// a client writing to a daemon that died still gets EPIPE and reports it, and
/// the daemon is never killed by a hung-up client. The client's only: a
/// daemon task that fails a print must not end the daemon.
fn end_on_a_failed_print() {
    let report = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let payload = info.payload();
        let message = payload
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| payload.downcast_ref::<&str>().copied())
            .unwrap_or("");
        // std's own message for these prints, which ends in the errno.
        let cause = message
            .strip_prefix("failed printing to stdout: ")
            .or_else(|| message.strip_prefix("failed printing to stderr: "));
        match cause {
            Some(cause) if cause.ends_with(&format!("(os error {})", libc::EPIPE)) => {
                die_of_sigpipe()
            }
            Some(cause) => {
                style::print_error(&format!("could not write output: {cause}"));
                std::process::exit(1);
            }
            None => report(info),
        }
    }));
}

/// What `clap::Error::exit` does, without discarding a failed write: help or
/// version that never arrived fails the command as any other output does.
fn exit_from_clap(e: &clap::Error) -> ! {
    match e.print() {
        Ok(()) => {}
        // A usage error goes to stderr, which cannot report its own failure.
        Err(_) if e.use_stderr() => {}
        Err(io) if io.kind() == std::io::ErrorKind::BrokenPipe => die_of_sigpipe(),
        Err(io) => {
            style::print_error(&format!("could not write output: {io}"));
            std::process::exit(1);
        }
    }
    std::process::exit(e.exit_code());
}

/// End the process as a write to a closed pipe does by default.
fn die_of_sigpipe() -> ! {
    // SAFETY: SIG_DFL installs no handler and runs no Rust code; an inherited
    // blocked mask is lifted for this thread so the signal lands; raise
    // delivers it here, which ends the process.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
        let mut pipe: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut pipe);
        libc::sigaddset(&mut pipe, libc::SIGPIPE);
        libc::pthread_sigmask(libc::SIG_UNBLOCK, &pipe, std::ptr::null_mut());
        libc::raise(libc::SIGPIPE);
    }
    // The signal did not end the process: exit as a shell would report it.
    std::process::exit(141);
}

/// Render a fatal error once, then exit non-zero.
///
/// `main` used to return `anyhow::Result<()>`, so every error `run_client` had
/// already rendered through `style::print_error` got rendered a SECOND time by
/// std's `Termination` impl — `Error: …` followed by thirty frames of
/// backtrace naming our own dependency paths, for conditions as ordinary as a
/// typo'd session name. That is issue #133, and it made every "not found" read
/// like a crash.
///
/// Exiting here means the `Termination` path is never reached, so the message
/// an operator sees is the one we chose to write. The backtrace stays
/// reachable behind the standard opt-in, for whoever is actually debugging.
fn report_fatal(e: &anyhow::Error) -> ! {
    use std::io::Write as _;

    let closed = e
        .chain()
        .filter_map(|c| c.downcast_ref::<style::OutputError>())
        .any(|o| o.0.kind() == std::io::ErrorKind::BrokenPipe);
    if closed {
        die_of_sigpipe();
    }

    // `{:#}` walks the anyhow chain onto one line; `print_error` owns the
    // marker, the colour and the NO_COLOR check.
    style::print_error(&format!("{e:#}"));

    if std::env::var_os("RUST_BACKTRACE").is_some_and(|v| !v.is_empty() && v != "0") {
        // anyhow's Debug is exactly what Termination used to print — chain
        // plus captured backtrace. Opted into, it is useful; unconditional,
        // it was the defect.
        //
        // Sanitized, and NOT optionally. The chain carries error text built
        // from untrusted input — a TOML parse diagnostic quotes the offending
        // source line verbatim, so a template containing a raw ESC replays it
        // straight at the operator's terminal (issue #104's whole class).
        // `safe_diagnostic` keeps `\n`/`\t`, which are this block's structure,
        // and escapes everything else. Asking for a backtrace is not consent
        // to be attacked by one.
        let _ = writeln!(
            std::io::stderr(),
            "\n{}",
            style::safe_diagnostic(&format!("{e:?}"))
        );
    }

    // `exit` runs no destructors, so anything buffered on the data channel
    // has to be flushed by hand or a partial `--format json` payload is lost.
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    std::process::exit(1);
}
