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
    let matches = cmd.get_matches();
    let args = Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());

    let result = if matches!(args.command, Some(Command::__daemon)) {
        // Internal daemon subcommand — called by auto-start. `--socket` is a
        // global arg, so the re-exec'd daemon sees whatever the client saw.
        run_daemon(args.socket.clone())
    } else {
        // Normal CLI client mode
        run_client(args)
    };

    if let Err(e) = result {
        report_fatal(&e);
    }
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
        eprintln!("\n{}", style::safe_diagnostic(&format!("{e:?}")));
    }

    // `exit` runs no destructors, so anything buffered on the data channel
    // has to be flushed by hand or a partial `--format json` payload is lost.
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    std::process::exit(1);
}
