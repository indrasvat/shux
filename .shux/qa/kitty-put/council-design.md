# DootSabha design council: issue #234 (kitty-put part 1)

Run: `dootsabha council --json` on the design, before coding (2026-10-31 17:29-17:42). Prompt: /tmp/shux234bin/council-design.md. Raw output: /tmp/shux234bin/council-design.json.

Providers: codex (ok), agy (ok)

## Chair synthesis (verbatim)

Ārya, the unified recommendation is **revise all four designs before treating them as implementation-ready**. The strongest agreement concerns shown-frame routing, graphics lifecycle separation, resize deduplication, and descriptor hygiene. The main disagreements resolve as follows.

This synthesis uses the supplied code reviews and a fresh check of upstream protocol documentation. It does not independently reproduce the failures or verify the current working tree.

**1. Kitty graphics — separate identity, placement, and geometry**

Adopt these requirements:

- Separate stored images from placements. Anonymous puts (`p=0` or omitted) append; a nonzero `(image_id, placement_id)` replaces that placement, preferably without changing its ordering.
- Handle retransmission invalidation in transmission processing, including transmit-only `a=t`.
- Include targeted deletion in minimum scope. Preserve the distinction between removing placements and freeing unused image data; visible-placement deletion must not erase unrelated scrollback.
- Carry source crop and destination geometry through composition, rasterization, and attach emission. Keep payload dimensions immutable.
- Preserve synchronized-output snapshots and mutation tracking when replacing, deleting, or evicting placements.
- Bound destination work before allocation, preserve placement limits, and refuse unsupported virtual placements.

**Resolve the scaling dispute in Codex’s favour:** current Kitty documentation explicitly requires aspect-preserving scaling, with letterboxing/pillarboxing when both `c` and `r` are supplied. Clipping to `min(source, destination)` is incompatible. Fix rasterization and emission together, then validate against real Kitty; agreement between nested shux paths alone cannot establish correctness. [Kitty layout rules](https://sw.kovidgoyal.net/kitty/graphics-protocol/#controlling-displayed-image-layout)

**Resolve `I=` as a scope decision:** image numbers are nonunique and address the newest matching image. Full support needs internal IDs, number lookup, and appropriate responses. Keeping APC silence means documenting incomplete compatibility or explicitly deferring support; it does not justify treating `I=` as an ordinary unique key. Number-based commands can operate without waiting for an ID response. [Image-number semantics](https://sw.kovidgoyal.net/kitty/graphics-protocol/#requesting-image-ids-from-the-terminal)

Memory accounting need not become a cross-grid reference-tracking framework. A bounded store plus independently enforced grid budgets can suffice, provided the aggregate retained-memory bound includes alternate and frozen grids. Store eviction alone cannot guarantee reclamation while placements retain `Arc`s.

**2. Attach resize — use one reconciliation path**

Adopt early SIGWINCH registration, preserve the exact hello size, and route every resize trigger through one helper that:

1. Reads the current terminal size.
2. Compares it with the last successfully sent size.
3. Sends changes and propagates failure.
4. Updates the cache only after success.

Treat crossterm resize events as triggers; their queued dimensions can be stale.

An immediate post-handshake reconciliation is inexpensive and makes startup convergence explicit, even if the registered signal already covers the normal race. The five-second interval is a resilience choice, **not a requirement established by these reviews**. Prefer the signal-driven design once startup, rapid-resize, busy-loop, and restoration tests pass; retain a persistent interval only for an identified recovery need.

Reuse the existing isolated PTY harness where its setup already satisfies the requirements. Do not describe existing isolation, terminal ownership, or output draining as missing without checking them.

**3. Descriptor hygiene — approve hardening; keep attribution qualified**

Close inherited descriptors during fresh daemon bootstrap after stdio redirection and before acquiring the instance lock. For child spawning, preserve stdio and Rust’s exec-error channel by applying CLOEXEC rather than closing arbitrary descriptors inside `pre_exec`.

Cover every production spawner: PTYs, plugins, status commands, git probes, and diagnostics. Use platform facilities or a complete strategy; arbitrary descriptor ceilings silently weaken the guarantee. Keep post-fork work async-signal-safe, handle enumeration failure explicitly, and measure spawn latency.

For nextest, **agy correctly identifies the post-exit distinction**: descriptors retained solely inside the exited test process cannot explain a subsequent pipe leak. Nextest detects output handles remaining open after test exit. An external holder—including a sibling child still before exec—can explain it. That supports the runner-inheritance hypothesis without proving the exact macOS race caused these reports. [Nextest leak detection](https://www.nexte.st/docs/features/leaky-tests/)

Use bounded sentinel-descriptor and EOF tests, verify the child or daemon remains alive when EOF arrives, test nonexistent-executable errors, and always kill/reap children. Retain LEAK visibility and seek the holder PID and pipe identity. Neither passing Linux CI nor a later descriptor guard establishes attribution.

**4. Mouse and wheel — use one shown-frame target throughout**

This is the clearest implementation direction:

- Resolve the target, containment, and cell mapping from the same shown `PaneView`.
- Remove the second live-geometry containment check for new application presses.
- Apply the policy to wheel forwarding, alternate-scroll arrows, scrollback, and copy-mode handoff.
- Consume expired stamped events and events targeting destroyed or currently ineligible panes before they can fall through into border dragging or selection.
- For unstamped clients, use the latest shown frame. Initialize shown views in tests instead of adding incomplete production fallbacks.
- Define active-window and zoom eligibility, preserve owned-gesture release handling, and update `active_pane_id` only after successful focus.
- Preserve Shift override, copy-mode, border-drag, and gesture precedence.

Test layout swaps, split/resize/zoom, window switches, pane destruction, expired stamps, differing pane mouse modes, and release after owner disappearance.

**Priority:** fix shown-frame routing first; treat descriptor cleanup as the highest-risk implementation; give graphics geometry and lifecycle changes their own focused validation. Before coding, anchor the findings to a named commit plus working-tree diff—the supplied reports sometimes conflate those snapshots.
