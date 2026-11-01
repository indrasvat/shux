//! A bounded interpreter that drives the recording registry through
//! overlapping starts, tees, stops, waits, owner loss, faults and shutdown,
//! and checks it. proptest runs it under `make test`; the libFuzzer target
//! `pane_record_lifecycle` runs it for long stretches.
//!
//! Safety properties hold after every operation. Liveness properties are
//! checked once every gate is open and shutdown has drained.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use shux_core::model::PaneId;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::pane_record::testing::{Fault, TestOpener};
use crate::pane_record::{EndedBy, Outcome, PaneRecordStatus, Recordings, requested_length};

const PANES: usize = 3;
const PATHS: usize = 3;
const OWNERS: usize = 3;
const MAX_OPS: usize = 160;
/// Small, so writers and then tees block, and backpressure is exercised.
const SINK_CAPACITY: usize = 16;
const CHANNEL_CAPACITY: usize = 2;

/// What one run reached, so a driver can tell the fuzzer still exercises
/// what it claims to.
#[derive(Clone, Copy, Debug, Default)]
pub struct Coverage {
    pub refused_starts: usize,
    pub complete_with_output: usize,
    pub aborted: usize,
    pub errored: usize,
    pub owner_ended: usize,
    pub shutdown_ended: usize,
    pub waited: usize,
}

impl std::ops::AddAssign for Coverage {
    fn add_assign(&mut self, o: Self) {
        self.refused_starts += o.refused_starts;
        self.complete_with_output += o.complete_with_output;
        self.aborted += o.aborted;
        self.errored += o.errored;
        self.owner_ended += o.owner_ended;
        self.shutdown_ended += o.shutdown_ended;
        self.waited += o.waited;
    }
}

/// Run the operations `data` encodes; panics on a broken property.
pub fn run(data: &[u8]) -> Coverage {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .start_paused(true)
        .build()
        .expect("runtime");
    runtime.block_on(async { Model::new().run(data).await })
}

#[derive(Clone)]
struct Started {
    id: uuid::Uuid,
    owner: Option<usize>,
    outcome: tokio::sync::watch::Receiver<Outcome>,
}

enum PtyCmd {
    Tee(Vec<u8>),
    Exit,
}

/// One chunk offered to one recording, and whether it reached the writer.
struct Offer {
    data: Vec<u8>,
    sent: bool,
}

struct Model {
    root: CancellationToken,
    opener: TestOpener,
    rec: Recordings,
    panes: Vec<PaneId>,
    paths: Vec<PathBuf>,
    _dir: tempfile::TempDir,
    owners: Vec<CancellationToken>,
    ptys: Vec<mpsc::UnboundedSender<PtyCmd>>,
    offers: Arc<StdMutex<HashMap<uuid::Uuid, Vec<Offer>>>>,
    /// Committed recordings, subscribed at commit so every outcome is seen
    /// however late it finalizes.
    started: Arc<StdMutex<Vec<Started>>>,
    starts: tokio::task::JoinSet<()>,
    waiters: Vec<(uuid::Uuid, tokio::task::JoinHandle<Option<Outcome>>)>,
    /// First finalized outcome seen per recording.
    finals: HashMap<uuid::Uuid, Outcome>,
    shut_down: bool,
    /// Recordings nothing had ended yet when shutdown began.
    open_at_shutdown: Vec<uuid::Uuid>,
    refused: Arc<std::sync::atomic::AtomicUsize>,
}

impl Model {
    fn new() -> Self {
        let root = CancellationToken::new();
        let opener = TestOpener::new();
        let rec =
            Recordings::with_opener(root.clone(), opener.opener(SINK_CAPACITY), CHANNEL_CAPACITY);
        let dir = tempfile::tempdir().expect("tempdir");
        let base = std::fs::canonicalize(dir.path()).expect("canonical tempdir");
        let paths = (0..PATHS).map(|i| base.join(format!("f{i}"))).collect();
        let panes: Vec<PaneId> = (0..PANES).map(|_| PaneId::new()).collect();
        let offers = Arc::new(StdMutex::new(HashMap::<uuid::Uuid, Vec<Offer>>::new()));
        let ptys = panes
            .iter()
            .map(|&pane| {
                let (tx, mut rx) = mpsc::unbounded_channel::<PtyCmd>();
                let (rec, offers, shutdown) = (rec.clone(), offers.clone(), root.clone());
                // The pane's PTY loop: one tee at a time, then its exit.
                tokio::spawn(async move {
                    while let Some(cmd) = rx.recv().await {
                        match cmd {
                            PtyCmd::Tee(data) => {
                                for admission in rec.admit(pane) {
                                    let id = admission.id();
                                    let sent = admission
                                        .send_bytes(data.clone(), Instant::now(), &shutdown)
                                        .await;
                                    offers.lock().expect("offers").entry(id).or_default().push(
                                        Offer {
                                            data: data.clone(),
                                            sent,
                                        },
                                    );
                                }
                            }
                            PtyCmd::Exit => rec.stop_pane(pane, EndedBy::PaneExit),
                        }
                    }
                });
                tx
            })
            .collect();
        Self {
            root,
            opener,
            rec,
            panes,
            paths,
            _dir: dir,
            owners: (0..OWNERS).map(|_| CancellationToken::new()).collect(),
            ptys,
            offers,
            started: Arc::new(StdMutex::new(Vec::new())),
            starts: tokio::task::JoinSet::new(),
            waiters: Vec::new(),
            finals: HashMap::new(),
            shut_down: false,
            open_at_shutdown: Vec::new(),
            refused: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    fn recording(&self, pick: u8) -> Option<uuid::Uuid> {
        let started = self.started.lock().expect("started");
        (!started.is_empty()).then(|| started[usize::from(pick) % started.len()].id)
    }

    async fn run(mut self, data: &[u8]) -> Coverage {
        let mut seq = 0u16;
        for op in data.chunks(3).take(MAX_OPS) {
            let [kind, a, b] = [op[0], *op.get(1).unwrap_or(&0), *op.get(2).unwrap_or(&0)];
            // Mostly a pane something is recording, where output matters.
            let live = self.rec.live();
            let pane = match live.get(usize::from(b) % live.len().max(1)) {
                Some((_, Some(p), _)) if a % 4 != 0 => {
                    self.panes.iter().position(|q| q == p).unwrap_or(0)
                }
                _ => usize::from(a) % PANES,
            };
            // Weighted: output and starts dominate, as they do in use.
            match kind % 20 {
                0 | 1 if !self.shut_down => self.start(pane, usize::from(b) % PATHS, a, b),
                2..=6 => {
                    seq += 1;
                    let mut chunk = vec![pane as u8, (seq >> 8) as u8, seq as u8];
                    chunk.resize(3 + usize::from(b % 120), b'.');
                    let _ = self.ptys[pane].send(PtyCmd::Tee(chunk));
                }
                7 => {
                    for admission in self.rec.admit(self.panes[pane]) {
                        admission.try_send_resize(80, 24, Instant::now());
                    }
                }
                8 => {
                    if let Some(id) = self.recording(b) {
                        self.rec.stop(id, EndedBy::Stop);
                    }
                }
                9 => {
                    if let Some(id) = self.recording(b)
                        && let Some((mut rx, _)) = self.rec.watch(id)
                    {
                        let waiter = tokio::spawn(async move {
                            rx.wait_for(|o| o.finalized).await.ok().map(|o| o.clone())
                        });
                        self.waiters.push((id, waiter));
                    }
                }
                10 => self.owners[usize::from(b) % OWNERS].cancel(),
                11 => {
                    let _ = self.ptys[pane].send(PtyCmd::Exit);
                }
                12 => {
                    // Abandon a start in flight: its caller is gone.
                    if !self.starts.is_empty() && b % 2 == 0 {
                        self.starts.abort_all();
                    }
                }
                13 | 14 => {
                    // A live recording's sink: pausing a finished one does nothing.
                    let target = live
                        .get(usize::from(a) % live.len().max(1))
                        .and_then(|(id, _, _)| self.opener.of(*id));
                    if let Some(sink) = target {
                        sink.set_draining(b % 3 == 0);
                    }
                }
                15 => self.opener.set_open(b % 2 == 0),
                16 => {
                    *self.opener.fault.lock().expect("fault") = match b % 4 {
                        0 => Fault::default(),
                        1 => Fault {
                            fail_after: Some(usize::from(a % 64)),
                            ..Fault::default()
                        },
                        2 => Fault {
                            panic_after: Some(usize::from(a % 64)),
                            ..Fault::default()
                        },
                        _ => {
                            self.opener
                                .fail_next_open
                                .store(true, std::sync::atomic::Ordering::SeqCst);
                            Fault::default()
                        }
                    };
                }
                17 => {
                    for _ in 0..=(b % 8) {
                        tokio::task::yield_now().await;
                    }
                }
                18 => tokio::time::advance(Duration::from_millis(u64::from(b) * 40)).await,
                // Rare mid-run: the run always ends with a shutdown anyway.
                19 if !self.shut_down && b % 4 == 0 => self.shutdown(),
                _ => {}
            }
            tokio::task::yield_now().await;
            self.check_safety();
        }
        self.finish().await
    }

    fn start(&mut self, pane: usize, path: usize, a: u8, b: u8) {
        let rec = self.rec.clone();
        let (pane_id, path_buf) = (self.panes[pane], self.paths[path].clone());
        let owner = a.is_multiple_of(3).then(|| usize::from(b) % OWNERS);
        let owner_token = owner.map(|o| self.owners[o].clone());
        let duration = b
            .is_multiple_of(5)
            .then(|| Duration::from_millis(u64::from(a) * 10));
        let started = self.started.clone();
        let refused = self.refused.clone();
        self.starts.spawn(async move {
            use crate::pane_record::RecordStartError::{PaneBusy, PathBusy};
            let prepared = match rec.prepare(&path_buf, true, Some(pane_id)).await {
                Ok(p) => p,
                Err(PaneBusy(_) | PathBusy(_)) => {
                    refused.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    return;
                }
                Err(_) => return,
            };
            let Ok((id, _)) = prepared.commit(pane_id, None) else {
                return;
            };
            let (outcome, _) = rec.watch(id).expect("a recording just committed");
            started
                .lock()
                .expect("started")
                .push(Started { id, owner, outcome });
            if let Some(token) = owner_token {
                rec.end_when(id, token.cancelled_owned(), EndedBy::OwnerDisconnected);
            }
            if let Some(after) = duration {
                rec.end_when(id, requested_length(after), EndedBy::Duration);
            }
        });
    }

    fn shutdown(&mut self) {
        self.shut_down = true;
        self.open_at_shutdown = self
            .started
            .lock()
            .expect("started")
            .iter()
            .filter(|s| s.outcome.borrow().ended_by.is_none())
            .map(|s| s.id)
            .collect();
        // As the daemon does: cancel, close admission, let every pane's PTY
        // task end (its recordings end as it does); `stop_all` comes later.
        self.root.cancel();
        self.rec.close_admission();
        for pty in &self.ptys {
            let _ = pty.send(PtyCmd::Exit);
        }
    }

    fn check_safety(&mut self) {
        let live = self.rec.live();
        for (i, (id, pane, path)) in live.iter().enumerate() {
            for (other, opane, opath) in &live[i + 1..] {
                assert!(
                    !(pane.is_some() && pane == opane),
                    "two live recordings on one pane: {id} and {other}"
                );
                assert_ne!(
                    path, opath,
                    "two live recordings on one path: {id} and {other}"
                );
            }
        }
        let started: Vec<_> = self.started.lock().expect("started").clone();
        for Started { id, owner, outcome } in started {
            let now = outcome.borrow().clone();
            if owner.is_none() {
                assert_ne!(
                    now.ended_by,
                    Some(EndedBy::OwnerDisconnected),
                    "an unowned recording ended with a connection"
                );
            }
            if now.finalized {
                let first = self.finals.entry(id).or_insert_with(|| now.clone());
                assert_eq!(*first, now, "a finalized outcome changed");
            }
        }
    }

    async fn finish(mut self) -> Coverage {
        // The daemon stops with whatever is stalled still stalled; then, for
        // liveness, every gate opens and every pending start resolves.
        if !self.shut_down {
            self.shutdown();
        }
        tokio::task::yield_now().await;
        self.check_safety();
        self.opener.set_open(true);
        *self.opener.fault.lock().expect("fault") = Fault::default();
        for (_, _, sink) in self.opener.opens.lock().expect("opens").iter() {
            sink.set_draining(true);
        }
        while self.starts.join_next().await.is_some() {}
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        self.rec.stop_all(EndedBy::DaemonShutdown);
        // Waiters resolve at finalization; drain retires entries after it.
        let unfinished = self.rec.drain(Duration::from_secs(600)).await;
        assert!(unfinished.is_empty(), "drain gave up on {unfinished:?}");
        let mut cover = Coverage {
            refused_starts: self.refused.load(std::sync::atomic::Ordering::SeqCst),
            waited: self.waiters.len(),
            ..Coverage::default()
        };
        for (id, waiter) in std::mem::take(&mut self.waiters) {
            let got = tokio::time::timeout(Duration::from_secs(600), waiter)
                .await
                .expect("a waiter on an ended recording never resolved")
                .expect("waiter task");
            let got = got.expect("a waiter saw its recording abandoned");
            assert!(got.finalized);
            if let Some(first) = self.finals.get(&id) {
                assert_eq!(*first, got, "a waiter saw a different outcome");
            }
            self.finals.entry(id).or_insert(got);
        }
        for Started { id, outcome, .. } in self.started.lock().expect("started").clone() {
            let now = outcome.borrow().clone();
            assert!(now.finalized, "a recording outlived shutdown: {now:?}");
            let first = self.finals.entry(id).or_insert_with(|| now.clone());
            assert_eq!(*first, now, "a finalized outcome changed");
        }
        for id in &self.open_at_shutdown {
            let cause = self.finals[id].ended_by;
            assert!(
                matches!(cause, None | Some(EndedBy::DaemonShutdown)),
                "a recording still open when shutdown began ended by {cause:?}"
            );
        }
        assert_eq!(self.rec.entry_count(), 0, "drain left entries behind");
        assert_eq!(self.rec.task_count(), 0, "drain left tasks behind");
        // Let each sink's reader take what its writer flushed.
        for _ in 0..64 {
            tokio::task::yield_now().await;
        }
        self.check_files();
        for o in self.finals.values() {
            match o.status {
                PaneRecordStatus::Complete if o.bytes_written > 0 => {
                    cover.complete_with_output += 1
                }
                PaneRecordStatus::Aborted => cover.aborted += 1,
                PaneRecordStatus::Error => cover.errored += 1,
                _ => {}
            }
            match o.ended_by {
                Some(EndedBy::OwnerDisconnected) => cover.owner_ended += 1,
                Some(EndedBy::DaemonShutdown) => cover.shutdown_ended += 1,
                _ => {}
            }
        }
        cover
    }

    /// Each finalized recording's file against what was offered to it.
    fn check_files(&self) {
        let started = self.started.lock().expect("started").clone();
        let offers = self.offers.lock().expect("offers");
        for Started { id, .. } in started {
            let outcome = &self.finals[&id];
            let sent: Vec<u8> = offers
                .get(&id)
                .into_iter()
                .flatten()
                .filter(|o| o.sent)
                .flat_map(|o| o.data.clone())
                .collect();
            let discarded = offers.get(&id).into_iter().flatten().any(|o| !o.sent);
            let Some(sink) = self.opener.of(id) else {
                continue;
            };
            let file = sink.bytes();
            match outcome.status {
                PaneRecordStatus::Complete => {
                    assert!(
                        !discarded,
                        "complete, yet an admitted chunk never reached the writer"
                    );
                    assert_eq!(file, sent, "a complete file is not what was admitted");
                    assert_eq!(outcome.bytes_written, file.len() as u64);
                }
                PaneRecordStatus::Aborted => {
                    assert_eq!(file, sent, "an aborted file is not what reached the writer");
                    assert_eq!(outcome.bytes_written, file.len() as u64);
                }
                PaneRecordStatus::Error => {
                    assert!(sent.starts_with(&file), "an errored file is not a prefix");
                    assert_eq!(
                        outcome.bytes_written,
                        file.len() as u64,
                        "an errored recording miscounts what landed"
                    );
                }
                PaneRecordStatus::Recording => panic!("finalized while still recording"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use proptest::test_runner::{Config, TestRunner};

    /// Runs the interpreter over generated operation sequences, then checks
    /// the runs between them reached every outcome the properties are about.
    #[test]
    fn the_recording_lifecycle_keeps_its_promises() {
        // A fixed seed: the interpreter is deterministic (one thread, paused
        // time, test sinks), so the cases and the coverage floor below are
        // the same on every machine. Open-ended search is `make fuzz-pane-record`.
        let mut runner = TestRunner::new_with_rng(
            Config {
                cases: 1024,
                failure_persistence: None,
                ..Config::default()
            },
            proptest::test_runner::TestRng::deterministic_rng(
                proptest::test_runner::RngAlgorithm::ChaCha,
            ),
        );
        let total = std::cell::Cell::new(super::Coverage::default());
        runner
            .run(&proptest::collection::vec(any::<u8>(), 0..480), |data| {
                let mut sum = total.get();
                sum += super::run(&data);
                total.set(sum);
                Ok(())
            })
            .unwrap();
        let c = total.get();
        for (what, n) in [
            ("refused overlapping starts", c.refused_starts),
            ("complete recordings with output", c.complete_with_output),
            ("aborted recordings", c.aborted),
            ("errored recordings", c.errored),
            ("recordings ended by their owner", c.owner_ended),
            ("recordings ended by shutdown", c.shutdown_ended),
            ("waiters", c.waited),
        ] {
            assert!(n >= 5, "the fuzzer reached only {n} {what}: {c:?}");
        }
    }
}
