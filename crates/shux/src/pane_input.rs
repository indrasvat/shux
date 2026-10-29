//! A pane's input channel, bounded in bytes as well as in chunks.
//!
//! A chunk holds its share of the pane's byte budget from the moment a sender
//! reserves room for it until the PTY task has written it, so a program that
//! stops reading holds every sender back once [`BUDGET`] unwritten bytes wait
//! for it, in the channel or in the PTY task. The daemon then holds at most the
//! budget and [`CONTROL_BUDGET`] for the pane, a few urgent reports bounded by
//! what they answer (see [`Lane::Urgent`]), and each waiting sender's own
//! chunk: the budgets bound what waits for the program, not how many senders
//! wait.
//!
//! Waiting reservations take room in order, so a large one is not starved by
//! small ones; while one waits, [`PaneWriter::try_reserve`] finds no room. Small
//! input that must not wait behind them takes room by [`Lane`].

use std::sync::Arc;

#[cfg(test)]
use tokio::sync::mpsc::error::TryRecvError;
use tokio::sync::mpsc::error::{SendError, TrySendError};
use tokio::sync::{AcquireError, OwnedSemaphorePermit, Semaphore, mpsc};

/// Unwritten bytes that may wait for one pane's program.
pub const BUDGET: usize = 16 << 20;
/// Unwritten bytes of control input that may wait beside [`BUDGET`].
pub const CONTROL_BUDGET: usize = 64 << 10;
/// What a chunk costs on top of its bytes until it is written: roughly its
/// allocation, so a flood of one-byte chunks is bounded too, with room for the
/// paste markers framing may add after room was reserved.
const CHUNK_COST: usize = 64;
/// Chunks that may wait to be received, urgent input aside.
const SLOTS: usize = 256;

/// One chunk of input, holding its unwritten bytes' share of the budget.
pub struct PaneInput {
    pub(crate) bytes: Vec<u8>,
    budget: Option<OwnedSemaphorePermit>,
}

impl PaneInput {
    /// `n` more bytes of this chunk were written: their budget goes back, all
    /// but the chunk's own cost, which goes back when the chunk is dropped.
    pub(crate) fn wrote(&mut self, n: usize) {
        if let Some(budget) = &mut self.budget {
            let held = budget.num_permits();
            let n = n.min(held.saturating_sub(CHUNK_COST));
            drop(budget.split(n));
        }
    }
}

#[cfg(test)]
impl From<Vec<u8>> for PaneInput {
    fn from(bytes: Vec<u8>) -> Self {
        Self {
            bytes,
            budget: None,
        }
    }
}

#[derive(Clone)]
struct Budget {
    permits: Arc<Semaphore>,
    limit: usize,
}

impl Budget {
    fn new(limit: usize) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(limit)),
            limit,
        }
    }

    /// A chunk larger than the whole budget takes all of it, so it goes once
    /// nothing else waits instead of never.
    fn cost(&self, len: usize) -> u32 {
        let cost = len.saturating_add(CHUNK_COST).min(self.limit);
        // Every budget here is far below u32::MAX.
        u32::try_from(cost).unwrap_or(u32::MAX)
    }

    fn try_take(&self, len: usize) -> Result<OwnedSemaphorePermit, TrySendError<()>> {
        self.permits
            .clone()
            .try_acquire_many_owned(self.cost(len))
            .map_err(|e| match e {
                tokio::sync::TryAcquireError::Closed => TrySendError::Closed(()),
                tokio::sync::TryAcquireError::NoPermits => TrySendError::Full(()),
            })
    }
}

/// Where a try-send looks for room once the main budget has none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lane {
    /// Nowhere: input that is worth dropping, such as motion or a wheel tick.
    Main,
    /// A separate, smaller budget, such as for a press.
    Control,
    /// That budget, then room outside every bound, slots included: input that
    /// must arrive and is bounded by something else, such as the release of a
    /// delivered press or a cancelled command's Ctrl-C.
    Urgent,
}

/// The sending side of a pane's input channel.
#[derive(Clone)]
pub struct PaneWriter {
    tx: mpsc::UnboundedSender<Queued>,
    slots: Arc<Semaphore>,
    budget: Budget,
    control: Budget,
}

/// A chunk in the channel, holding its slot until it is received.
struct Queued {
    input: PaneInput,
    _slot: Option<OwnedSemaphorePermit>,
}

/// The PTY task's side of a pane's input channel.
pub struct PaneInputRx {
    rx: mpsc::UnboundedReceiver<Queued>,
}

/// Room for one chunk: a slot in the channel and its bytes' budget, either
/// absent for urgent input.
pub struct InputPermit {
    tx: mpsc::UnboundedSender<Queued>,
    slot: Option<OwnedSemaphorePermit>,
    budget: Option<OwnedSemaphorePermit>,
}

/// A pane's input channel with the default bounds.
pub fn channel() -> (PaneWriter, PaneInputRx) {
    channel_with(SLOTS, BUDGET)
}

/// A pane's input channel holding at most `slots` chunks and `budget`
/// unwritten bytes, urgent input aside.
pub fn channel_with(slots: usize, budget: usize) -> (PaneWriter, PaneInputRx) {
    let (tx, rx) = mpsc::unbounded_channel();
    let (budget, control) = (Budget::new(budget), Budget::new(CONTROL_BUDGET));
    let writer = PaneWriter {
        tx,
        slots: Arc::new(Semaphore::new(slots)),
        budget,
        control,
    };
    (writer, PaneInputRx { rx })
}

impl PaneWriter {
    /// Room for a chunk of `len` bytes, waiting for it. Fails once the pane
    /// is gone.
    pub async fn reserve(&self, len: usize) -> Result<InputPermit, SendError<()>> {
        let room = async {
            let cost = self.budget.cost(len);
            let budget = self.budget.permits.clone().acquire_many_owned(cost).await?;
            let slot = self.slots.clone().acquire_owned().await?;
            Ok::<_, AcquireError>((slot, budget))
        };
        let (slot, budget) = tokio::select! {
            biased;
            () = self.tx.closed() => return Err(SendError(())),
            room = room => room.map_err(|_| SendError(()))?,
        };
        Ok(self.permit(Some(slot), Some(budget)))
    }

    /// [`Self::reserve`] without waiting.
    pub fn try_reserve(&self, len: usize) -> Result<InputPermit, TrySendError<()>> {
        self.try_reserve_from(&self.budget, len)
    }

    /// [`Self::try_reserve`], then wherever `lane` allows: for small input
    /// that must not wait behind input still waiting for room.
    pub fn try_reserve_in(&self, len: usize, lane: Lane) -> Result<InputPermit, TrySendError<()>> {
        let mut permit = self.try_reserve(len);
        if lane != Lane::Main && matches!(permit, Err(TrySendError::Full(()))) {
            permit = self.try_reserve_from(&self.control, len);
        }
        if lane == Lane::Urgent && matches!(permit, Err(TrySendError::Full(()))) {
            permit = Ok(self.permit(None, None));
        }
        permit
    }

    fn try_reserve_from(
        &self,
        budget: &Budget,
        len: usize,
    ) -> Result<InputPermit, TrySendError<()>> {
        if self.tx.is_closed() {
            return Err(TrySendError::Closed(()));
        }
        let budget = budget.try_take(len)?;
        let slot = self
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| TrySendError::Full(()))?;
        Ok(self.permit(Some(slot), Some(budget)))
    }

    fn permit(
        &self,
        slot: Option<OwnedSemaphorePermit>,
        budget: Option<OwnedSemaphorePermit>,
    ) -> InputPermit {
        InputPermit {
            tx: self.tx.clone(),
            slot,
            budget,
        }
    }

    #[cfg(test)]
    pub async fn send(&self, bytes: Vec<u8>) -> Result<(), SendError<Vec<u8>>> {
        match self.reserve(bytes.len()).await {
            Ok(permit) => {
                permit.send(bytes);
                Ok(())
            }
            Err(_) => Err(SendError(bytes)),
        }
    }

    #[cfg(test)]
    pub fn try_send(&self, bytes: Vec<u8>) -> Result<(), TrySendError<Vec<u8>>> {
        match self.try_reserve(bytes.len()) {
            Ok(permit) => {
                permit.send(bytes);
                Ok(())
            }
            Err(TrySendError::Full(())) => Err(TrySendError::Full(bytes)),
            Err(TrySendError::Closed(())) => Err(TrySendError::Closed(bytes)),
        }
    }
}

impl InputPermit {
    /// Queues `bytes`, or drops them if the pane is gone.
    pub fn send(self, bytes: Vec<u8>) {
        let input = PaneInput {
            bytes,
            budget: self.budget,
        };
        let _ = self.tx.send(Queued {
            input,
            _slot: self.slot,
        });
    }

    /// Whether this is room in `writer`'s channel.
    pub fn is_for(&self, writer: &PaneWriter) -> bool {
        self.tx.same_channel(&writer.tx)
    }
}

impl PaneInputRx {
    /// The next chunk, holding its budget until it is written.
    pub async fn recv_input(&mut self) -> Option<PaneInput> {
        self.rx.recv().await.map(|queued| queued.input)
    }

    /// The next chunk's bytes, giving its budget back.
    #[cfg(test)]
    pub async fn recv(&mut self) -> Option<Vec<u8>> {
        self.rx.recv().await.map(|queued| queued.input.bytes)
    }

    #[cfg(test)]
    pub fn try_recv(&mut self) -> Result<Vec<u8>, TryRecvError> {
        self.rx.try_recv().map(|queued| queued.input.bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_chunk_holds_its_budget_until_written() {
        let (tx, mut rx) = channel_with(8, 1000);
        tx.try_send(vec![0; 400]).unwrap();
        tx.try_send(vec![0; 400]).unwrap();
        assert!(matches!(
            tx.try_send(vec![0; 400]),
            Err(TrySendError::Full(_))
        ));

        let mut first = rx.recv_input().await.unwrap();
        assert!(
            matches!(tx.try_send(vec![0; 400]), Err(TrySendError::Full(_))),
            "received is not written"
        );
        first.wrote(300);
        tx.try_send(vec![0; 200]).unwrap();
        first.wrote(100);
        drop(first);
        tx.try_send(vec![0; 200]).unwrap();
    }

    #[tokio::test]
    async fn a_chunk_larger_than_the_budget_goes_once_nothing_else_waits() {
        let (tx, mut rx) = channel_with(8, 1000);
        tx.try_send(vec![1; 10]).unwrap();
        let big = tokio::spawn({
            let tx = tx.clone();
            async move { tx.send(vec![2; 5000]).await }
        });
        let small = rx.recv_input().await.unwrap();
        assert_eq!(small.bytes, vec![1; 10]);
        drop(small);
        big.await.unwrap().unwrap();
        assert_eq!(rx.recv().await.unwrap().len(), 5000);
    }

    #[tokio::test]
    async fn waiting_input_keeps_its_place() {
        let (tx, mut rx) = channel_with(8, 1000);
        let held = {
            tx.try_send(vec![0; 900]).unwrap();
            rx.recv_input().await.unwrap()
        };
        let big = tokio::spawn({
            let tx = tx.clone();
            async move { tx.send(vec![1; 500]).await }
        });
        shux_testkit::wait_until_async("the large send waits", || {
            let tx = tx.clone();
            async move {
                tokio::task::yield_now().await;
                tx.budget.permits.available_permits() == 0
            }
        })
        .await;
        assert!(
            matches!(tx.try_send(vec![2; 1]), Err(TrySendError::Full(_))),
            "small input jumped a waiting send"
        );
        tx.try_reserve_in(1, Lane::Control).unwrap().send(vec![3]);
        assert_eq!(rx.recv().await.unwrap(), vec![3], "control input waited");
        drop(held);
        big.await.unwrap().unwrap();
        assert_eq!(rx.recv().await.unwrap(), vec![1; 500]);
    }

    #[tokio::test]
    async fn each_lane_looks_only_as_far_as_it_may() {
        let (tx, _rx) = channel_with(4096, 1000);
        tx.try_send(vec![0; 900]).unwrap();
        assert!(matches!(
            tx.try_reserve_in(100, Lane::Main),
            Err(TrySendError::Full(()))
        ));
        while let Ok(permit) = tx.try_reserve_in(100, Lane::Control) {
            permit.send(vec![1; 100]);
        }
        assert!(matches!(
            tx.try_reserve_in(100, Lane::Control),
            Err(TrySendError::Full(()))
        ));
        tx.try_reserve_in(100, Lane::Urgent)
            .expect("urgent input finds room with both budgets spent")
            .send(vec![2; 100]);
    }

    #[tokio::test]
    async fn urgent_input_arrives_in_order_with_every_slot_taken() {
        let (tx, mut rx) = channel();
        let mut queued = 0;
        while let Ok(permit) = tx.try_reserve_in(1, Lane::Control) {
            permit.send(vec![0]);
            queued += 1;
        }
        tx.try_reserve_in(1, Lane::Urgent)
            .expect("urgent input waits for no slot")
            .send(vec![1]);
        for _ in 0..queued {
            assert_eq!(rx.recv().await.unwrap(), vec![0]);
        }
        assert_eq!(rx.recv().await.unwrap(), vec![1]);
    }

    #[tokio::test]
    async fn a_waiting_sender_fails_once_the_pane_is_gone() {
        let (tx, mut rx) = channel_with(8, 1000);
        tx.try_send(vec![0; 900]).unwrap();
        let held = rx.recv_input().await.unwrap();
        let waiting = tokio::spawn({
            let tx = tx.clone();
            async move { tx.send(vec![1; 500]).await }
        });
        drop(rx);
        assert!(waiting.await.unwrap().is_err());
        assert!(matches!(tx.try_send(vec![1]), Err(TrySendError::Closed(_))));
        drop(held);
    }
}
