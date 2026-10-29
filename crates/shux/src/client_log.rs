//! Where the client's logs go: stderr, except while the attach UI owns the
//! terminal. Then they are held and written once [`HoldGuard`] drops, so a
//! log line can never paint over the screen the UI is drawing.

use std::io::Write;
use std::sync::{Mutex, PoisonError};

/// Bytes held per attach; events past it are counted and dropped.
const HOLD_LIMIT: usize = 64 * 1024;

#[derive(Default)]
struct Held {
    bytes: Vec<u8>,
    dropped: usize,
}

static HELD: Mutex<Option<Held>> = Mutex::new(None);

/// The `tracing` writer for the client.
pub struct ClientLogWriter;

impl Write for ClientLogWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut held = HELD.lock().unwrap_or_else(PoisonError::into_inner);
        match held.as_mut() {
            Some(held) if held.bytes.len() + buf.len() <= HOLD_LIMIT => {
                held.bytes.extend_from_slice(buf);
                Ok(buf.len())
            }
            Some(held) => {
                held.dropped += 1;
                Ok(buf.len())
            }
            None => {
                drop(held);
                std::io::stderr().write(buf)
            }
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        std::io::stderr().flush()
    }
}

/// Holds the client's logs until dropped.
#[must_use]
pub struct HoldGuard(());

pub fn hold() -> HoldGuard {
    *HELD.lock().unwrap_or_else(PoisonError::into_inner) = Some(Held::default());
    HoldGuard(())
}

impl HoldGuard {
    /// Writes the held logs before the process dies, waiting at most `limit`
    /// for a stderr that has stopped reading. A write still blocked then is
    /// left to die with the process.
    pub fn release_within(self, limit: std::time::Duration) {
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            drop(self);
            let _ = done_tx.send(());
        });
        let _ = done_rx.recv_timeout(limit);
    }
}

impl Drop for HoldGuard {
    fn drop(&mut self) {
        let held = HELD.lock().unwrap_or_else(PoisonError::into_inner).take();
        let Some(held) = held else { return };
        let mut stderr = std::io::stderr().lock();
        let _ = stderr.write_all(&held.bytes);
        if held.dropped > 0 {
            let _ = writeln!(
                stderr,
                "[{} more log lines from while attached were dropped]",
                held.dropped
            );
        }
        let _ = stderr.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn held_bytes() -> Option<(usize, usize)> {
        HELD.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(|h| (h.bytes.len(), h.dropped))
    }

    #[test]
    fn logs_are_held_while_attached_and_released_after() {
        let guard = hold();
        ClientLogWriter.write_all(b"one\n").expect("held write");
        assert_eq!(held_bytes(), Some((4, 0)));
        ClientLogWriter
            .write_all(&vec![b'x'; HOLD_LIMIT])
            .expect("over the limit is still accepted");
        assert_eq!(
            held_bytes(),
            Some((4, 1)),
            "an event past the limit is dropped whole"
        );
        drop(guard);
        assert_eq!(held_bytes(), None);
    }

    /// A signal exit must not wait on a terminal that stopped reading: with
    /// stderr a full pipe nobody drains, releasing still returns.
    #[test]
    fn releasing_before_a_signal_exit_never_blocks_on_stderr() {
        use std::os::fd::AsFd;
        let (read, write) = nix::unistd::pipe().expect("pipe");
        let flags = nix::fcntl::fcntl(&write, nix::fcntl::FcntlArg::F_GETFL).expect("getfl");
        let nonblocking =
            nix::fcntl::OFlag::from_bits_truncate(flags) | nix::fcntl::OFlag::O_NONBLOCK;
        nix::fcntl::fcntl(&write, nix::fcntl::FcntlArg::F_SETFL(nonblocking)).expect("setfl");
        while nix::unistd::write(&write, &[0u8; 4096]).is_ok() {}
        nix::fcntl::fcntl(
            &write,
            nix::fcntl::FcntlArg::F_SETFL(nix::fcntl::OFlag::from_bits_truncate(flags)),
        )
        .expect("restore blocking");
        // nextest runs each test in its own process, so taking over fd 2 here
        // reaches no other test.
        let saved = nix::unistd::dup(std::io::stderr().as_fd()).expect("dup stderr");
        nix::unistd::dup2_stderr(&write).expect("dup2 onto stderr");

        let guard = hold();
        ClientLogWriter
            .write_all(b"held line\n")
            .expect("held write");
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            guard.release_within(std::time::Duration::from_millis(200));
            let _ = done_tx.send(());
        });
        let released = done_rx.recv_timeout(shux_testkit::BUDGET).is_ok();
        nix::unistd::dup2_stderr(&saved).expect("restore stderr");
        drop(read);
        assert!(released, "releasing the held logs blocked on a full stderr");
    }
}
