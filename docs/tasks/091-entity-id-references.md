# 091 — `pane list` printed an id that every other pane command rejected

**Status:** Done
**Priority:** High (the documented agent loop was unfollowable from its own first step; issue #120)
**Milestone:** M3 polish
**Depends On:** issue #88 fix (session name-or-UUID resolution) — this generalises it
**Touches:** `crates/shux-core/src/idref.rs` (new), `crates/shux-core/src/graph.rs`,
`crates/shux-core/src/lib.rs`, `crates/shux-rpc/src/error.rs`,
`crates/shux/src/main.rs`, `crates/shux/src/cli.rs`, `crates/shux/src/style.rs`,
`crates/shux/tests/id_prefix_resolution.rs` (new),
`.shux/scripts/issue_120_evidence.sh` (new),
`Makefile` (`test-id-refs`, `test-id-refs-evidence`)
**Adversarial review:** 4 parallel agents on disjoint surfaces, all driving the
real binary — see "Found by adversarial review" below

---

## Problem (issue #120)

`shux pane list` prints the pane id truncated to 8 characters. Nothing accepted
that form back.

```
$ shux pane list -s v4
b57c601b        /home/user/shux         sh -c sleep 300

$ shux pane glance b57c601b --text-only
✗ glance failed: invalid_params (code -32602)        # exit 2

$ shux pane capture -s v4 -p b57c601b
✗ invalid pane_id format                             # exit 1

$ shux pane glance b57c601b-5f61-4bc7-b411-2eb7e44fe6ff --text-only
✓ glance b57c601b rev 1 80×24 cursor (0,0) visible alt_screen no
```

The last line is the shape of the problem: `glance` echoes the short form back
in its own success output, so the id it prints is not an id it would accept.
The full UUID is reachable only via `--format json`.

`session list` had the same asymmetry (a short id column nothing consumed) and
`window list` the opposite one: `--window`'s help said "window id or index",
but the CLI only ever matched an index or a title, so a UUID read out of
`window list --format json` was rejected, and the human listing printed no id
at all.

Reproduced at `e856793` (v0.46.7) through the real binary; RED receipt in
`.shux/out/issue-120/red-receipt.txt` (8/8 new end-to-end tests failing).

### Why it matters

`pane list` is the only place a human gets a pane id from. The documented lens
loop — `lens run` → `pane wait-settled` → `pane glance` → `pane diff` — starts
with a listing, and the id it hands you is one the next step refuses. Agents,
the intended audience of that loop, hit it on their first call.

---

## Decision

Accept **the full UUID or any unambiguous prefix of it**, git-style. The short
form is what everything already displays, so making it resolve is the change
that needs no new convention.

One resolver, `shux_core::idref`, owns the rule; the daemon and the CLI both
route through it so they cannot drift.

1. **A complete UUID is returned verbatim and never looked up.** Every form
   `Uuid::parse_str` accepts counts. Whether the entity exists stays the
   handler's question, exactly as before — which is what makes the whole change
   additive: no call that worked before behaves differently now.
2. **Anything else is a prefix**: trimmed, hyphens removed, lowercased, then
   4..=31 hex digits. Four is git's floor for abbreviated SHAs; three out of a
   32-character space is one part in 4096, close enough to "any typo hits
   something" to be a footgun.
3. **Exactly one match resolves.** None is `not_found`. More than one is
   refused with the colliding ids named, in both the message and machine-
   readable `data.candidates` — picking one would be a coin flip on the
   caller's behalf.

Precedence where a name also exists (sessions, windows): **an exact match beats
a partial one.** Exact name, or full UUID, wins over an id prefix. Between the
two exact forms the pre-existing issue-#88 rule stands: the id wins, with a
warning. Windows keep index-first, then title, then id — so no selector that
resolved before resolves differently.

---

## Found by adversarial review, and fixed here

Four agents drove the real binary against this branch on disjoint surfaces.
Every finding below was reproduced independently before being believed.

* **A wrong-typed id parameter silently retargeted the call.** `pane_id` was
  read with `params.get(k).and_then(|v| v.as_str())`, which cannot tell
  "absent" from "present but a number". So `{"pane_id": 12345, "session_id":
  "..."}` fell through to the active-pane fallback and **succeeded** — zooming,
  resizing, retitling or typing into a pane the caller never named. Reached 16
  methods. Pre-existing, but this task owns both resolvers and rewrote them.
  Present-but-not-a-string is now `invalid_params`; absent and explicit `null`
  still mean "use the active one".
* **`session kill` / `rename` / `save` never got the resolver**, while
  `session kill --help` was reworded by this very task to promise short ids.
  `session kill` is where the documented loop *ends*, so the loop was
  unfollowable from its last step for the same reason as its first.
* **`pane watch` kept a client-side `Uuid` guard** that rejected short ids —
  even though the daemon's `pane.output.watch` was re-plumbed (a `GraphHandle`
  threaded into `register_events_methods`) specifically to accept them. Its
  `--session` flag was also documented as validating pane membership and did
  not; it does now, and returns the canonical id.
* **The e2e suite's identity assertions were vacuous.** The fixture had one
  pane in one window, so "resolved the short id" and "ignored the argument and
  used the only pane" were indistinguishable — reverting the resolver to always
  return the active pane still passed. The fixture is now two panes with
  different screen content plus a second window, every test drives the pane
  that is *not* first, and mutations are read back to prove they landed there.
* **The PNG assertion checked file size.** A blank 80×24 grid is far larger
  than the 1000-byte floor. It now decodes the image and counts
  non-background pixels.
* **Coverage was shaped around what was implemented, not what was promised** —
  `watch`, `record`, `run`, `resize`, `split` and the session verbs were in
  neither the test nor the evidence script, which is how the three defects
  above passed a green suite. Both now drive every pane verb that takes an id.
* **`graph_error_to_rpc` put a whole sentence in `data.id`**, so a missing pane
  rendered as `pane 'pane not found: <uuid>' not found`.
* **`stale_revision` threw away the answer.** `pane diff --since <old>` printed
  the bare code twice while `data` already carried the revisions that *are*
  diffable.
* **A nonexistent session blamed window resolution** — `pane list -s <unknown
  uuid>` said "could not determine active window". Newly visible because the
  message fix below stopped swallowing it.
* **`lens run` printed two unlabelled ids.** Step one of the loop handed you
  `2686a718 eb9c6c3e` with no way to tell session from pane.
* Dead API removed (`resolve_window_ref_in_session`, `RefKind::param()` — the
  latter would have been *wrong* where used, since `window.rename` and friends
  take a parameter called `id`); a 32-hex string with misplaced hyphens is now
  malformed rather than a "prefix", matching the documented 4..=31 range; and
  `"----"` no longer reports "it is empty".

## Also fixed here (same error path, found while fixing the above)

Both reproduced against the real binary before being believed.

* **A failed reference said nothing.** The CLI composes its own not-found
  errors ("session 'nosuch' not found", "window 'nope' not found in session",
  "pane X does not belong to session Y") and `rpc_display` discarded every one
  of them, printing the contentless `resource not found`. That is what a
  mistyped id produced — and it would have swallowed the new ambiguity and
  prefix errors too.
* **`name_conflict` never rendered.** `rpc_display`'s name-conflict arm was
  keyed on `-32003`, which is `auth_required`; the real code is `-32007`. A
  duplicate session name printed the raw `RPC error -32007: name_conflict`
  while the colliding name sat unused in `data`, and an auth failure would have
  rendered as a name conflict.
* **Egress guard on rendered errors.** Echoing the caller's own text back
  (above) put a live `ESC ]0;` on a path to the terminal. `rpc_display` is the
  single funnel for every RPC error the CLI prints, so the guard now sits
  there, covering the `detail` and `data` paths that were already unguarded.
* **`window list` printed no id.** Added as a trailing column in both text and
  plain output, so the three columns scripts already parse keep their
  positions.

Filed separately (genuinely unrelated): `--cmd` is documented as a shell
command but is split on whitespace, so `--cmd "printf x; sleep 300"` execs
`printf` with `sleep` as an argument and no error is reported.

---

## Testing matrix

| Level | Where | Covers |
|---|---|---|
| Unit (pure) | `shux-core/src/idref.rs` | 22 tests: every UUID spelling, case, hyphen placement, the 4-character floor, empty/non-hex/over-long, not-found, ambiguity ordering + capping + wording, control-character escaping |
| Unit (wire) | `shux/src/main.rs` | RPC parameter resolution for pane/window/session short ids; `RefError` → `RpcError` mapping incl. ambiguity `data.candidates`; the second pane parameter of `pane.swap` naming itself |
| Unit (client) | `shux/src/cli.rs` | `-s` name-beats-prefix precedence, session ambiguity, the 4-char floor, `-w` index/title/UUID/prefix precedence, pane membership returning the canonical id, `rpc_display` rendering (including the `stale_revision` and `name_conflict` arms) + egress guard |
| End to end | `shux/tests/id_prefix_resolution.rs` | Real binary, real daemon, real PTYs. Fixture is two panes with different screen content plus a second window, so "resolved the id" is distinguishable from "used the active one". Covers every pane verb that takes an id (including `watch` as a real stream and `record` to a file), every session verb, window-by-id, wrong-typed parameters, exit codes 2 vs 3, and the error text |

Ambiguity is not reachable through the real binary — ids are random v4 UUIDs, so
forcing a shared 4-hex prefix would take on the order of 256 live PTY panes. It
is pinned deterministically one layer down, over hand-built snapshots, in the
layer that owns the decision.

Run: `make test-id-refs` for the suites, `make test-id-refs-evidence
BASE_BIN=<pre-fix binary>` for the A/B round trip against the real binary —
**17 round-trip failures at `e856793`, 25/25 pass here**, with the refusals
holding in both arms so the baseline cannot pass vacuously either.

### Falsifiability

Each check was proven able to fail before being trusted:

| Injected defect | Caught by |
|---|---|
| Ambiguity resolves to the first hit | 5 core tests |
| `MIN_PREFIX_LEN` = 1 | 3 core tests |
| Prefix matching drops case normalization | 1 core test |
| Hyphens not stripped | 2 core tests |
| Resolver ignores `pane_id` and returns the active pane | 11/11 end-to-end tests |
| Wrong-typed `pane_id` falls through to the active pane again | exactly 1 end-to-end test, the one written for it |
| (the bug itself, at `e856793`) | 8/8 end-to-end tests |

The fifth row is the one that matters most. Under the ORIGINAL single-pane
fixture that same injection passed almost everything — which is why the fixture
was rebuilt with two panes carrying different screen content and a second
window. The sixth row is its complement: a targeted injection must kill the
test written for it and leave the rest green, or the suite is not localising
anything.

---

## Acceptance criteria

- [x] The id `pane list` prints is accepted by `glance`, `capture`,
      `wait-settled`, `checkpoint`, `diff`, `wait-for`, `title`, `send-keys`,
      `set-size`, `zoom`, `resize`, `run`, `snapshot`, `split`, `focus`,
      `swap`, `kill`, `watch`, `record`, and `pane.output.watch` — each
      exercised end to end, not asserted by inspection.
- [x] The id `session list` prints is accepted by `-s` on every session verb,
      including `kill`, `rename` and `save`.
- [x] A PRESENT id parameter of the wrong JSON type is `invalid_params`, never
      a silent fallback to the active pane; an absent or null one still means
      "use the active one".
- [x] `window list` prints an id, and `-w` accepts it — short form and full
      UUID both.
- [x] A short id and the full UUID address the same entity.
- [x] Malformed stays `invalid_params` (exit 2); an unmatched prefix is
      `not_found` (exit 3); a collision is refused with candidates.
- [x] No input that resolved before resolves differently.
- [x] Errors name what was missing, and are inert on a terminal.
- [x] `make check` green.
