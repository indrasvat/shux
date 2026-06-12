# 066 — Lossless Pane Output Recording

**Status:** Done

## Goal

Fix issue #70: provide a lossless pane byte-stream capture primitive suitable
for absence-of-bytes assertions, while making the existing sampled
`pane.output.watch` semantics explicit to CLI consumers.

## Problem

`pane.output.watch` is intentionally sampled at the PTY output source. It is
useful for live observation, but it is unsound for audits that assert a byte
sequence was not emitted. Dropped bytes can make a broken renderer look clean.

## Planned Approach

- Keep `pane.output.watch` sampled and low-overhead. It remains a live
  observation stream, not an audit transcript.
- Add `pane.record.start` / `pane.record.stop` as a daemon-owned recorder that
  tees raw PTY bytes immediately after a successful read, before VT processing
  and before the sampled data-plane coalescer can drop bytes.
- Model each recorder as a state machine:
  `recording -> complete | error | aborted`. `stop` returns the status,
  `bytes_written`, `lossless`, and any error message so audit scripts can fail
  closed.
- Use intentional backpressure for the recording path. If the destination cannot
  keep up, the pane can be slowed; this is the cost of byte-exact absence
  assertions. The send path is cancellable by pane shutdown and marks the
  recorder aborted instead of silently claiming success.
- Enforce one active recorder per pane in v1 to bound the backpressure blast
  radius.
- Resolve CLI `--to` paths on the client side before RPC so files land where
  the caller expects, not relative to daemon cwd.
- Default to create-new output files. `--force` truncates through an
  `O_NOFOLLOW` open on Unix so the final path component cannot be a symlink.
- Support daemon-side `duration_ms` so bounded scripts do not depend solely on
  the client surviving long enough to call `stop`.

## Contracts

- Start boundary: bytes already read by the PTY task before `record.start`
  returns are not captured. Emit the stimulus under audit only after start
  succeeds.
- Stop boundary: `record.stop` includes chunks handed to the recorder before the
  recorder is stopped; bytes read after the stop boundary are not included.
- File format: raw concatenated PTY bytes in source order for a single pane.

## Verification

- Focused RPC tests for byte-exact high-volume recording, duplicate recorder
  rejection, existing-file protection, and daemon-side duration expiry.
- CLI parser coverage for `shux pane record`.
- Real-tool smoke with `gh-hound`, `vivecaka`, and rich TUI screenshots through
  shux.
