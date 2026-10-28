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
use std::time::{Duration, Instant};

/// How long a wait may take before the test fails.
///
/// Bounds failure only, so it is deliberately far above any honest wait: the
/// slowest condition in the suite (a daemon booting under 12-way contention on
/// a macOS runner) settles in a few seconds. nextest terminates a test at 120s.
pub const BUDGET: Duration = Duration::from_secs(30);

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

/// [`wait_for`] with a budget other than [`BUDGET`].
#[track_caller]
pub fn wait_for_within<T>(budget: Duration, what: &str, probe: impl FnMut() -> Option<T>) -> T {
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

    #[tokio::test(flavor = "current_thread")]
    async fn poll_async_gives_up_with_none() {
        let probed = poll_async(Duration::from_millis(5), || async { None::<()> }).await;
        assert!(probed.is_none());
    }
}
