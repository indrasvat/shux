//! Event-based waits for shux tests.
//!
//! A test waits for a *condition*, never for an amount of time. Every helper
//! here re-checks a probe until it reports the condition, and the deadline only
//! decides when to give up and fail: a fast machine finishes as soon as the
//! condition holds, a loaded one keeps waiting. See `docs/testing.md`.
//!
//! This crate holds the workspace's only sanctioned test-side sleep (the poll
//! interval below). `clippy.toml` disallows `std::thread::sleep`,
//! `tokio::time::sleep` and `tokio::time::sleep_until` everywhere else.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How long a wait may take before the test fails.
///
/// Bounds failure only, so it is deliberately far above any honest wait: the
/// slowest condition in the suite (a daemon booting under 12-way contention on
/// a macOS runner) settles in a few seconds. nextest terminates a test at 120s.
pub const BUDGET: Duration = Duration::from_secs(30);

/// [`BUDGET`] in milliseconds, for RPC `timeout_ms` parameters.
pub const BUDGET_MS: u64 = BUDGET.as_millis() as u64;

const FIRST_INTERVAL: Duration = Duration::from_millis(1);
const MAX_INTERVAL: Duration = Duration::from_millis(25);

fn next_interval(current: Duration, deadline: Instant) -> Duration {
    let remaining = deadline.saturating_duration_since(Instant::now());
    current.min(remaining)
}

/// Re-run `probe` until it returns `Some`, or `budget` runs out (`None`).
///
/// The probe always runs at least once, and once more at the deadline, so a
/// condition that holds is never reported as a timeout.
pub fn poll<T>(budget: Duration, mut probe: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = Instant::now() + budget;
    let mut interval = FIRST_INTERVAL;
    loop {
        if let Some(found) = probe() {
            return Some(found);
        }
        if Instant::now() >= deadline {
            return None;
        }
        #[expect(
            clippy::disallowed_methods,
            reason = "the poll interval between probes of a condition"
        )]
        std::thread::sleep(next_interval(interval, deadline));
        interval = (interval * 2).min(MAX_INTERVAL);
    }
}

/// [`poll`] for a boolean condition.
pub fn poll_until(budget: Duration, mut cond: impl FnMut() -> bool) -> bool {
    poll(budget, || cond().then_some(())).is_some()
}

/// Wait up to [`BUDGET`] for `probe` to return `Some`; panic naming `what` if
/// it never does.
#[track_caller]
pub fn wait_for<T>(what: &str, probe: impl FnMut() -> Option<T>) -> T {
    wait_for_within(BUDGET, what, probe)
}

#[track_caller]
fn wait_for_within<T>(budget: Duration, what: &str, probe: impl FnMut() -> Option<T>) -> T {
    let started = Instant::now();
    match poll(budget, probe) {
        Some(found) => found,
        None => panic!("timed out after {:?} waiting for {what}", started.elapsed()),
    }
}

/// Wait up to [`BUDGET`] for `cond`; panic naming `what` if it never holds.
#[track_caller]
pub fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    wait_for(what, || cond().then_some(()));
}

/// Async [`poll`]. Yields to the runtime between probes, so it is safe on a
/// current-thread runtime whose other tasks must run for the condition to hold.
///
/// The interval is a tokio sleep: under a paused clock with auto-advance held
/// off it never wakes. Wait on the blocking pool with [`poll`] there.
pub async fn poll_async<T, F, Fut>(budget: Duration, mut probe: F) -> Option<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Option<T>>,
{
    let deadline = Instant::now() + budget;
    let mut interval = FIRST_INTERVAL;
    loop {
        if let Some(found) = probe().await {
            return Some(found);
        }
        if Instant::now() >= deadline {
            return None;
        }
        #[expect(
            clippy::disallowed_methods,
            reason = "the poll interval between probes of a condition"
        )]
        tokio::time::sleep(next_interval(interval, deadline)).await;
        interval = (interval * 2).min(MAX_INTERVAL);
    }
}

/// Async [`poll_until`].
pub async fn poll_until_async<F, Fut>(budget: Duration, mut cond: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    poll_async(budget, || {
        let fut = cond();
        async move { fut.await.then_some(()) }
    })
    .await
    .is_some()
}

/// Async [`wait_for`].
#[track_caller]
pub fn wait_for_async<'a, T, F, Fut>(what: &'a str, probe: F) -> impl Future<Output = T> + 'a
where
    T: 'a,
    F: FnMut() -> Fut + 'a,
    Fut: Future<Output = Option<T>> + 'a,
{
    let caller = std::panic::Location::caller();
    async move {
        let started = Instant::now();
        match poll_async(BUDGET, probe).await {
            Some(found) => found,
            None => panic!(
                "{caller}: timed out after {:?} waiting for {what}",
                started.elapsed()
            ),
        }
    }
}

/// Async [`wait_until`].
#[track_caller]
pub fn wait_until_async<'a, F, Fut>(what: &'a str, mut cond: F) -> impl Future<Output = ()> + 'a
where
    F: FnMut() -> Fut + 'a,
    Fut: Future<Output = bool> + 'a,
{
    wait_for_async(what, move || {
        let fut = cond();
        async move { fut.await.then_some(()) }
    })
}

/// SIGTERM `pid` and wait for it to exit; SIGKILL it if it outlasts
/// [`BUDGET`]. Returns whether it exited on SIGTERM, so a teardown can still
/// fail the test without leaking the process.
pub fn terminate(pid: i32) -> bool {
    terminate_within(pid, BUDGET)
}

fn terminate_within(pid: i32, grace: Duration) -> bool {
    use nix::sys::signal::{Signal, kill};
    let target = nix::unistd::Pid::from_raw(pid);
    let gone = || kill(target, None).is_err() || is_zombie(pid);
    let _ = kill(target, Signal::SIGTERM);
    if poll_until(grace, gone) {
        return true;
    }
    let _ = kill(target, Signal::SIGKILL);
    poll_until(BUDGET, gone);
    false
}

/// An exited process its parent has not reaped still answers `kill(pid, 0)`.
fn is_zombie(pid: i32) -> bool {
    #[cfg(target_os = "linux")]
    let state = std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|stat| {
            stat.get(stat.rfind(')')? + 1..)?
                .trim_start()
                .chars()
                .next()
        });
    #[cfg(not(target_os = "linux"))]
    let state = std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .ok()
        .and_then(|out| {
            String::from_utf8_lossy(&out.stdout)
                .trim_start()
                .chars()
                .next()
        });
    state == Some('Z')
}

/// A FIFO for a child to wait at (`read -r _ < gate`) until [`open_gate`].
pub fn gate(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    nix::unistd::mkfifo(&path, nix::sys::stat::Mode::S_IRWXU).expect("mkfifo");
    path
}

/// Let one child waiting at `gate` through. The open is non-blocking, so it
/// succeeds only once a reader is at the gate, and a gate nobody reaches fails
/// the wait instead of hanging the test.
#[track_caller]
pub fn open_gate(gate: &Path) {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut fifo = wait_for("a reader to wait at the gate", || {
        std::fs::OpenOptions::new()
            .write(true)
            .custom_flags(nix::fcntl::OFlag::O_NONBLOCK.bits())
            .open(gate)
            .ok()
    });
    fifo.write_all(b"go\n").expect("open the gate");
}

/// [`open_gate`] on the blocking pool, so it also works under a paused clock
/// whose auto-advance is held off.
pub async fn open_gate_async(gate: &Path) {
    let gate = gate.to_owned();
    tokio::task::spawn_blocking(move || open_gate(&gate))
        .await
        .expect("the gate opener panicked");
}

/// CPU time the calling thread has used. Unlike the wall clock it does not
/// advance while the thread is descheduled, so load elsewhere cannot skew a
/// cost ratio.
pub fn thread_cpu_ns() -> u128 {
    let t = nix::time::clock_gettime(nix::time::ClockId::CLOCK_THREAD_CPUTIME_ID)
        .expect("CLOCK_THREAD_CPUTIME_ID is supported on every target shux builds for");
    t.tv_sec() as u128 * 1_000_000_000 + t.tv_nsec() as u128
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn poll_returns_as_soon_as_the_condition_holds() {
        let calls = Cell::new(0);
        let found = poll(BUDGET, || {
            calls.set(calls.get() + 1);
            (calls.get() == 3).then_some("ready")
        });
        assert_eq!(found, Some("ready"));
        assert_eq!(calls.get(), 3);
    }

    #[test]
    fn poll_probes_once_more_at_the_deadline() {
        let calls = Cell::new(0);
        assert!(!poll_until(Duration::ZERO, || {
            calls.set(calls.get() + 1);
            false
        }));
        assert_eq!(calls.get(), 1, "a zero budget still probes once");
        assert!(poll_until(Duration::ZERO, || true));
    }

    #[test]
    #[should_panic(expected = "waiting for the impossible")]
    fn wait_for_names_what_it_waited_on() {
        wait_for_within(Duration::from_millis(5), "the impossible", || None::<()>);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn poll_async_lets_other_tasks_make_the_condition_true() {
        let (tx, rx) = tokio::sync::watch::channel(false);
        tokio::spawn(async move {
            tokio::task::yield_now().await;
            let _ = tx.send(true);
        });
        wait_until_async("the spawned task to run", || {
            let rx = rx.clone();
            async move { *rx.borrow() }
        })
        .await;
    }

    /// A child that has printed `ready`, with SIGTERM ignored or not.
    fn ready_child(ignore_term: bool) -> std::process::Child {
        use std::io::BufRead;
        let trap = if ignore_term { "trap '' TERM; " } else { "" };
        let mut child = std::process::Command::new("sh")
            .args(["-c", &format!("{trap}echo ready; exec sleep 300")])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("spawn sh");
        let mut line = String::new();
        std::io::BufReader::new(child.stdout.take().expect("piped stdout"))
            .read_line(&mut line)
            .expect("read the ready line");
        assert_eq!(line, "ready\n");
        child
    }

    #[test]
    fn terminate_counts_an_unreaped_child_as_gone() {
        let mut child = ready_child(false);
        assert!(
            terminate(child.id() as i32),
            "a child that died on SIGTERM and awaits its parent's wait was reported alive"
        );
        child.wait().expect("reap");
    }

    #[test]
    fn terminate_kills_what_ignores_sigterm() {
        use std::os::unix::process::ExitStatusExt;
        let mut child = ready_child(true);
        assert!(
            !terminate_within(child.id() as i32, Duration::ZERO),
            "SIGTERM was ignored"
        );
        let status = child.wait().expect("reap");
        assert_eq!(status.signal(), Some(nix::libc::SIGKILL));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn poll_async_gives_up_with_none() {
        let probed = poll_async(Duration::from_millis(5), || async { None::<()> }).await;
        assert!(probed.is_none());
    }
}
