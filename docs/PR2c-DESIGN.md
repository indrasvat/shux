# PR 2c — Sampled `pane.output` events with data-plane separation

**Status:** Design + minimal implementation (deferred from PR 2a per
Codex + Gemini council finding).

## The problem

PR 2a (`events.watch` + `events.history`) ships a typed control-plane
event bus: session/window/pane lifecycle, focus, zoom, exit, etc.
Subscribers tail it for safe, low-volume routing.

The PR 2a council surfaced two reasons **PTY output must NOT flow
through this same bus**:

1. **Secret leak.** `events.history` snapshots the last N events in a
   shared ring buffer. ANY caller of `events.history` (or any future
   plugin with the `events.read` permission) gets to read those bytes.
   PTY output regularly contains passwords typed at sudo prompts,
   `.env` files cat-ed to the screen, API keys returned from `gh
   auth login`, JWT tokens, AWS access keys. Once it lands in
   history, the only way to scrub it is to evict it through normal
   ring-buffer rotation.
2. **DoS for control events.** A `cargo build`, `tail -F large.log`,
   `npm install`, or `ls -R /` can each push tens of thousands of
   chunks per second. With a 4096-slot broadcast channel and an
   8192-event history, a single noisy pane drowns every
   `session.killed` / `pane.exited` / `window.renamed` event the
   agent actually cares about. Subscribers lag, get a `Lagged(N)`
   error, and either miss control events or have to re-snapshot the
   whole graph.

## The chosen shape — Option A (data-plane separation)

A separate broadcast channel inside `EventBus`:

```rust
struct EventBusInner {
    // Control plane (PR 2a — unchanged).
    sender: broadcast::Sender<Event>,
    history: RwLock<EventHistory>,
    seq_counter: AtomicU64,
    config: EventBusConfig,

    // Data plane (PR 2c — NEW).
    data_plane: broadcast::Sender<PaneOutputEvent>,
    data_seq_counter: AtomicU64,
}
```

Key invariants:

- `PaneOutputEvent` is **NEVER** written to `history`. Subscribers can
  only ever read live events, not a snapshot. This removes the
  secret-leak vector entirely — bytes that arrived before
  `pane.output.watch` was called are unreachable.
- `events.history` and `events.watch` are unchanged. They only see
  the control plane.
- A separate `data_seq_counter` so data-plane gap detection is
  independent of the control plane's seq.
- The data-plane channel is sized larger than the control plane
  (e.g. 16k) because PTY traffic is naturally burstier.
- **Rate-limiting at the publisher.** The per-pane PTY task tracks a
  `last_published_at: Instant` and only publishes if `now - last >=
  sample_interval` (default 100ms). High-volume panes coalesce
  into one chunk per interval — the council's "DoS" concern can't
  fire because the channel can't grow faster than ~10 chunks/sec
  per pane.
- The control plane's `pane.output` variant (currently in
  `EventData::PaneOutput`) is **removed**. It was added in PR 2a but
  never wired to actual PTY output. Removing it now is cheap since
  no caller depends on it, and keeping a dead variant where the same
  name lives on a different bus is a recipe for future confusion.

### Why not Option B (sampled notifier + pull)

Option B was the alternative the council brainstormed: emit a tiny
`pane.output_dirty` event (no payload) on the control plane and have
the subscriber pull via `pane.capture`. Reasons we picked A:

- **Notifier still floods control plane.** Even no-payload events
  pushed at 60Hz from 4 panes is 240 events/sec into the same ring
  the agent reads `session.created` from. Same DoS shape, just
  smaller per-event.
- **Extra round-trip per chunk.** Each pull requires a second RPC
  call; the agent's polling rate becomes the latency floor. Option A
  pushes once.
- **`pane.capture` is for grid scrapes, not streams.** It returns the
  current viewport, not the bytes since last call. Wiring "since
  last call" requires keeping a per-pane cursor inside the daemon,
  which is most of the work Option A does anyway.

## The wire format

```rust
/// A chunk of PTY output for one pane. Lives ONLY on the data plane.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaneOutputEvent {
    pub seq: u64,
    pub pane_id: PaneId,
    pub window_id: WindowId,
    pub session_id: SessionId,
    pub timestamp: SystemTime,
    /// Base64-encoded raw bytes that were written to the PTY since
    /// the last published chunk. May include partial UTF-8 sequences
    /// — consumers must decode lossily or buffer.
    pub bytes: String,
    /// Whether this chunk was sampled (some bytes dropped between
    /// `last_published_at` and now) or lossless (verbatim).
    pub sampled: bool,
}
```

## RPC surface

New method on the daemon:

```
pane.output.watch
  params:
    pane_id: String        — required; data plane is per-pane
    from_seq: Option<u64>  — resume from a known data-plane seq
    timeout_ms: u64        — long-poll deadline, clamped 100..=30000
    limit: Option<u64>     — return early after N chunks
  result:
    chunks: Vec<PaneOutputEvent>  — chunks ≥ from_seq, capped by limit
    next_seq: u64                 — pass back as from_seq next call
```

Unlike `events.watch`, **there is no history snapshot**. `from_seq`
older than the channel's tail returns whatever chunks are still in
the receiver's buffer; no `gap` field is surfaced because the data
plane is fundamentally lossy by design (sampled at the source).

## CLI

```
shux pane watch -s <session> [-p <pane>] [--sample-ms 100]
```

Long-polls `pane.output.watch` in a loop, base64-decodes each chunk,
prints to stdout. This is a live observation stream, not a transcript
recorder: high-volume panes can be sampled before publication. Use
`shux pane record --to FILE` when absence-of-bytes or byte-exact audit
semantics matter.

## Out of scope for v1

- **Persistent recording.** PR 2c intentionally did not provide an
  on-disk capture path. Task 066 later added `pane.record.start` /
  `pane.record.stop` and `shux pane record` as the lossless recorder
  primitive for byte-exact transcripts.
- **Multi-pane subscription in one call.** v1 requires one
  `pane.output.watch` per pane. Multiplexing inside one call is a
  later optimization once we see actual agent demand.
- **Server-side filtering on byte content.** Agents that want
  "match on the prompt regex" can do it client-side. The daemon's
  job is to deliver bytes, not parse them.

## Test plan

- Unit: data-plane bus drops bytes when no subscriber, doesn't
  pollute control-plane history, sampling rate-limits at the source.
- Integration: `pane.output.watch` returns chunks, `events.history`
  does not include them, `events.watch` does not see them either.
- Visual: `shux pane watch` in one pane shows live output from a
  `echo loop` running in another.
