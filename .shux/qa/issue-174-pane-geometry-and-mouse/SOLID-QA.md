VERDICT: PASS

# SOLID VT QA — issue #174, pane geometry and mouse forwarding

## 1. Change under audit

| | |
|---|---|
| Issue | #174 — declare pixel geometry to panes; forward button events |
| Branch | `claude/issue-174-fqhgsn` |
| Base | `9cf1bd5` |
| HEAD at verdict | `f58ca39` |
| Prior audit | `dc4b2b1` → `VERDICT: FAIL` (2 × P1) |
| Delta re-audited | `dc4b2b1..f58ca39` |

This is a **delta re-audit**, scoped by the coordinator. Section 3 states
exactly which layers were re-taken at `f58ca39` and which are carried forward
from the `dc4b2b1` audit.

`f58ca39` changes no product source relative to `6f8cf00`
(`git diff --stat 6f8cf00..f58ca39 -- crates/` is empty); it commits the pixel
metrics and makes `issue_174_pixel_ab.sh` record repo-relative paths. Every
product-code measurement below was taken against a binary built from source
identical to `f58ca39`.

## 2. Both prior P1s — closed, verified independently

### P1 #1 — `snapshot.rs` hardcoded `BorderStyle::Rounded` (CLOSED)

Re-taken. Fill a 60×20 pane so every column carries a truecolor background,
with a unique sentinel in the **last** column; compare the pane grid tail
against the composed window-snapshot PNG.

Pane grid row 2, all three binaries:
`AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAE` — 59 × `A`
(bg `0,200,0`) then `E` (bg `255,0,255`) in column 60.

`window snapshot --cols 60 --rows 20` under `border_style = "none"`, row 1
sampled at every cell centre (image 540×380 = 60×20 × the declared 9×19 box in
all three cases):

| Binary | Row-1 colour runs | Reads |
|---|---|---|
| base `9cf1bd5` | `(106,181,215)` @0, `(16,16,24)` @1–58, `(106,181,215)` @59 | rounded outline drawn, pane content absent — **cropped** |
| sabotage (HEAD + `Rounded` put back) | identical to base | **cropped** |
| **HEAD** | `(0,200,0)` @0–58, `(255,0,255)` @59 | full grid, no outline — **not cropped** |

HEAD's column 59 is the magenta sentinel that the grid tail ends on. The
measurement is proven able to detect the defect by two independent controls
(base and sabotage), so the green is not vacuous.

### P1 #2 — nothing could catch it coming back (CLOSED)

`crates/shux/tests/window_snapshot_border_style.rs` was proven red against the
reintroduced hardcode, in a separate worktree at `HEAD` — the shared checkout
was never modified.

Sabotage applied and **verified present in the compiled artifact** before
believing anything (the coordinator's warning about a silently-matching-nothing
edit was taken literally):

```
/tmp/qa174-sabotage/crates/shux/src/snapshot.rs:265: let _border_style = shux_ui::BorderStyle::parse(...)
/tmp/qa174-sabotage/crates/shux/src/snapshot.rs:294:            shux_ui::BorderStyle::Rounded,
source mtime 03:06:20  <  binary mtime 03:10:42
```

The sabotage binary was then independently shown to reproduce base's crop
(table above) before the test was run against it.

```
Summary [2.839s] 2 tests run: 1 passed, 1 failed
FAIL  window_snapshot_honours_border_style_none
  first column is not the pane's fill ((106, 181, 215, 255)); the snapshot
  inset for an outline the user turned off
PASS  window_snapshot_still_draws_the_default_outline
```

Red for the **right reason**: the failure names the outline inset, not a
generic mismatch. The control arm stays green under sabotage, so the pair is
not simply failing on everything. Both tests are listed by
`cargo nextest list -p shux --test window_snapshot_border_style` and are not
among the run's 2 skipped tests.

## 3. Testing matrix

Re-taken at `f58ca39` (this audit):

| Layer | Result | Evidence |
|---|---|---|
| Unit + integration | PASS — `2191 tests run: 2191 passed, 2 skipped` in 111s | `.shux/out/qa-reaudit-174/make-test.log` |
| Regression test proven red | PASS | §2, P1 #2; `/tmp/qa174-sab-test.log` |
| Crop measurement + 2 controls | PASS | §2, P1 #1 |
| Pixel comparison — pane, `rounded` (default) | PASS 0/513000, exact 0/0 | `pixel/pixel-render-parity-rounded-pane.json` |
| Pixel comparison — window, `rounded` (default) | PASS 0/738720, exact 0/0 | `pixel/pixel-render-parity-rounded-window.json` |
| Pixel comparison — pane, `none` | PASS 0/513000, exact 0/0 | `pixel/pixel-render-parity-none-pane.json` |
| Non-blank guard proven able to fail | PASS | blank PNG → exit 1; missing PNG → exit 1 |
| Colour probes | PASS — truecolor + indexed + basic in every A/B payload; 1342 (pane) / 2093 (window) distinct colours both sides | `.shux/out/qa-reaudit-174/` logs |
| Leaked daemons | PASS — zero after every run; tree clean | `ps -eo pid,args \| grep -c "[s]hux daemon"` → 0 |
| DootSabha design / implementation | PASS via documented fallback | `council-substitution.md` |
| Independent QA verdict | This report | — |

**Carried forward from the `dc4b2b1` audit, not re-taken** (the coordinator
scoped this run as a delta; `git diff dc4b2b1..f58ca39` touches
`crates/shux-vt/src/parser.rs` **test module only**, `help_overlay.rs`,
`snapshot.rs`, `attach.rs` and the harness scripts — nothing in
`shux-raster`, `shux-vt` grid/parser production code, or `capture.rs`):

| Layer | Carried-forward result | From |
|---|---|---|
| VT corpus goldens — 19 comparisons | PASS, all 0/0 | `dc4b2b1` |
| DECRQM — 18 modes byte-identical | PASS | `dc4b2b1` |
| Rich-TUI matrix | PASS — 5 ok, 2 with cursor ground truth | `dc4b2b1` |
| Raw replay fixtures | PASS | `dc4b2b1` |
| Three harnesses proven able to fail | PASS | `dc4b2b1` |
| Mutation battery | 30/30 killed | `dc4b2b1` |
| Unicode width / default colours / cursor / alt screen / scroll regions | PASS, untouched | `dc4b2b1` |
| shux automation at 80×24 / 120×40 / 200×60 | PASS | `dc4b2b1` |

`make test-vt-corpus` was deliberately **not** re-run: nothing since `dc4b2b1`
touches `shux-vt` production code or `shux-raster`, and running it rewrites 19
tracked PNGs. No tracked file outside this scope directory was modified by this
audit; `git status` is clean apart from this evidence.

The `border_style = "none"` **window** arm has no equality metric by design:
base ignored the setting, so the two are *supposed* to differ. That direction
is asserted by `issue_174_snapshot_style_check.sh` with the base binary as a
control, and independently by §2's three-way table. Agreed as correct.

## 4. Pixel metrics — regenerated, not trusted

The three committed metric JSONs were **regenerated from scratch in this
audit** with the base binary built from `9cf1bd5`, and are byte-identical to
the committed files:

```
IDENTICAL pixel-render-parity-rounded-pane.json
IDENTICAL pixel-render-parity-rounded-window.json
IDENTICAL pixel-render-parity-none-pane.json
```

All three: `"status": "pass"`, `max_pixel_diff_ratio: 0.0`,
`max_mean_channel_delta: 0.0`, `changed_pixels: 0`. Both sides were shown
non-blank first (1342 / 2093 distinct colours, 2.657% / 5.565% non-background),
so the equality is not two blank images agreeing.

No PNG is committed alongside them. Neither side of any comparison is a tracked
baseline — both are scratch renders under `.shux/out/issue-174/pixel/` — so
per `.shux/qa/README.md` the metric JSON stands alone and no `*-actual.png` is
owed. The gate agrees with that reading and does not want a baseline PNG added.

## 5. Screenshot matrix

| Viewport | Command | Screenshot | Baseline | Status |
|---|---|---|---|---|
| 100×30 | colour-probed payload, `border_style=rounded` | `.shux/out/issue-174/pixel/head.png` | `base.png` (scratch, base binary) | PASS 0/513000 |
| 120×38 window | same, composed window | `.shux/out/issue-174/pixel/head-window.png` | `base-window.png` | PASS 0/738720 |
| 100×30 | same, `border_style=none` | `.shux/out/issue-174/pixel/head.png` | `base.png` | PASS 0/513000 |
| 60×20 window | crop sentinel, `border_style=none` | `/tmp/qa174-win-none-{head,base,sabotage}.png` | base + sabotage binaries | PASS (see §2) |

Every PNG in this table was opened and inspected pixel-by-pixel in this audit,
not merely asserted to exist.

## 6. Findings

No P0 or P1. Both prior P1s are closed. Non-blocking observations are listed in
the audit reply for the PR description as explicitly deferred scope.

## 7. Residual risk

- The rich-TUI matrix, VT corpus and mutation battery are carried forward from
  `dc4b2b1` rather than re-run. Justified by the delta touching no
  `shux-vt`/`shux-raster` production code, but it is a carry-forward, not a
  fresh measurement, and is named as such.
- The council substitution was verified by its effects in the diff, not by
  observing transcripts.
- Live-attach rendering has no pixel A/B arm; the compositor code is unchanged
  in the delta and the shared `pane_viewport` rule is unit-pinned, but attach
  is proven by grid/text assertions rather than pixels.

## 8. Cleanup

- Zero shux daemons running at verdict time.
- Sabotage worktree `/tmp/qa174-sabotage` removed; the shared checkout was
  never modified by this audit.
- No tracked file outside `.shux/qa/issue-174-pane-geometry-and-mouse/` was
  written. `.shux/qa/073-shux-vt-corpus-regression-harness/` is untouched.
