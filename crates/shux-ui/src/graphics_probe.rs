//! Ask the user's terminal, once per attach, whether it can draw images.
//!
//! The same handshake `kitten icat` and zellij use, and the one shux answers on
//! the pane side: an `a=q` query followed by a primary device attributes
//! request as a sentinel. A DA1 reply arriving with no APC reply before it
//! means the terminal ignored the graphics command, so it cannot draw one.
//!
//! Emitting without asking is not merely wasteful. Measured with a shux attach
//! running inside tmux 3.4: the emitter's own continuation header became the
//! tmux window title -- `Gq=2,m=0;AP+HAP+H...` where the base build showed
//! `vm` -- rewritten once per frame by any pane that redraws. An outer
//! multiplexer is not a terminal that quietly ignores an APC block.
//!
//! The probe owns its raw mode and nothing else. An earlier version moved
//! `TerminalGuard::enter` ahead of the attach handshake so the reply would not
//! be echoed, which left the user in the alternate screen with `ISIG` off and
//! no working Ctrl-C if the daemon stalled. Raw mode here spans the probe and
//! not one instruction more.

use std::io::{IsTerminal, Read, Write};
use std::os::fd::AsFd;
use std::time::{Duration, Instant};

/// Arbitrary, and echoed back in the reply so a stale response cannot pass for
/// this one.
const PROBE_ID: u32 = 4207;

const QUERY: &[u8] = b"\x1b_Gi=4207,a=q,s=1,v=1,f=24,t=d;AAAA\x1b\\\x1b[c";

/// True only if the terminal answered the graphics query.
///
/// Enters and leaves raw mode around the exchange, because the reply is line
/// buffered and echoed otherwise. Returns false on any I/O error or timeout:
/// the whole point is to stay silent unless support is proven.
pub fn probe(timeout: Duration) -> bool {
    if !std::io::stdin().is_terminal() || crossterm::terminal::enable_raw_mode().is_err() {
        return false;
    }
    let answered = ask(timeout);
    // Restored whatever the answer was, including on an early return above.
    let _ = crossterm::terminal::disable_raw_mode();
    answered
}

fn ask(timeout: Duration) -> bool {
    let mut stdout = std::io::stdout();
    if stdout.write_all(QUERY).is_err() || stdout.flush().is_err() {
        return false;
    }
    read_reply(&mut std::io::stdin(), timeout)
}

/// Split out so a test can drive it without a terminal.
fn read_reply<R: Read + AsFd>(input: &mut R, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    let ok = format!("_Gi={PROBE_ID};OK");
    let mut seen = Vec::new();
    let mut buf = [0u8; 256];
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() || !wait_readable(input.as_fd(), left) {
            return false;
        }
        match input.read(&mut buf) {
            Ok(0) => return false,
            Ok(n) => seen.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return false,
        }
        if find(&seen, ok.as_bytes()) {
            return true;
        }
        // DA1 is the sentinel: the terminal has answered everything it is
        // going to, so a missing APC reply is now a definite "no".
        if seen.contains(&b'c') && find(&seen, b"\x1b[?") {
            return false;
        }
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// `poll` rather than a read timeout: stdin is the client's real terminal and
/// must not be left in non-blocking mode for the input thread that follows.
fn wait_readable(fd: std::os::fd::BorrowedFd<'_>, timeout: Duration) -> bool {
    let mut fds = [nix::poll::PollFd::new(fd, nix::poll::PollFlags::POLLIN)];
    let ms = u16::try_from(timeout.as_millis()).unwrap_or(u16::MAX);
    matches!(nix::poll::poll(&mut fds, ms), Ok(n) if n > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Seek;

    /// A file is `Read + AsFd` and always polls readable, which is what the
    /// reply parser needs; a pipe would need a writer thread to say the same.
    fn reply(bytes: &[u8]) -> std::fs::File {
        let mut f = tempfile::tempfile().unwrap();
        f.write_all(bytes).unwrap();
        f.rewind().unwrap();
        f
    }

    #[test]
    fn an_apc_ok_before_da1_is_support() {
        let mut f = reply(b"\x1b_Gi=4207;OK\x1b\\\x1b[?62;c");
        assert!(read_reply(&mut f, Duration::from_millis(500)));
    }

    #[test]
    fn da1_alone_is_no_support() {
        let mut f = reply(b"\x1b[?62;1;6c");
        assert!(!read_reply(&mut f, Duration::from_millis(500)));
    }

    #[test]
    fn another_probes_id_does_not_answer_for_ours() {
        let mut f = reply(b"\x1b_Gi=31;OK\x1b\\\x1b[?62;c");
        assert!(!read_reply(&mut f, Duration::from_millis(500)));
    }

    #[test]
    fn a_closed_stream_is_no_support() {
        let mut f = reply(b"");
        assert!(!read_reply(&mut f, Duration::from_millis(50)));
    }

    /// The case a real terminal produces: it never answers at all. A regular
    /// file always polls readable and reads EOF, so it exercises the wrong
    /// branch -- this needs a pipe whose write end stays open with nothing in
    /// it, which is the only way to reach the deadline.
    #[test]
    fn a_terminal_that_never_answers_times_out() {
        let (read_end, _write_end) = nix::unistd::pipe().unwrap();
        let mut f = std::fs::File::from(read_end);
        let started = Instant::now();
        assert!(!read_reply(&mut f, Duration::from_millis(120)));
        assert!(
            started.elapsed() >= Duration::from_millis(100),
            "returned in {:?}; it cannot have waited for the deadline",
            started.elapsed()
        );
    }
}
