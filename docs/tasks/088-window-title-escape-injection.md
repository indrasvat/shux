# 088 — window titles bypass `sanitize_title()` and reach the terminal with control bytes intact

**Status:** Done
**Priority:** High (security — terminal escape injection via untrusted template / RPC input; issue #104)
**Milestone:** M3 polish
**Depends On:** 027 (pane titles), 033 (templates / `state apply`)
**Touches:** `crates/shux-core/src/model.rs`, `crates/shux-core/src/graph.rs`,
`crates/shux/src/style.rs`, `crates/shux/src/cli.rs`, `crates/shux/src/main.rs`,
`crates/shux/src/statusbar_build.rs`,
`crates/shux/tests/window_title_escape_injection.rs` (new)

---

## Problem (issue #104)

Window titles are stored raw and printed raw to the operator's terminal, so a title
carrying escape sequences **executes them**. `shux state apply` against a template from
an untrusted repo is the realistic vector: TOML forbids raw control bytes in a basic
string, but its own `\uXXXX` escapes decode to real control bytes before shux ever sees
them.

`sanitize_title()` (`model.rs:342`) — strip `char::is_control()`, trim, clamp to 64 —
already exists and is wired **only** to the pane manual (`model.rs:268`) and OSC
(`model.rs:278`) paths. Every window path bypasses it — and, as adversarial review later
showed, so did the pane path that derives a title from the command or cwd basename.

### Reproduced with the real binary (`0.46.3`, `312abf1`)

```sh
printf '[session]\nname = "ev2"\n\n[[windows]]\ntitle = "\\u001B]0;PWNED-W0\\u0007deploy"\n\n[[windows.panes]]\ncommand = ["bash"]\n' > evil.toml
shux state apply evil.toml
shux window list -s ev2 | cat -v
#   0       ^[]0;PWNED-W0^Gdeploy    1        ← raw ESC + BEL on the operator's terminal
```

| # | Surface | Pre-fix observed |
|---|---|---|
| 1 | `state apply` template title → `window list` (text) | `0\t^[]0;PWNED-W0^Gdeploy\t1` |
| 2 | `window list --format plain` | same raw bytes |
| 3 | `window create --name $'\e]0;X\a'` | `✓ Created window 'C^[]0;PWNED-CREATE^GD'` |
| 4 | `window rename --name $'\e]0;X\a'` | `✓ Renamed window '…' -> 'R^[]0;PWNED-RENAME^GS'` |
| 5 | `window focus` | `✓ Focused window '^[^G'` |
| 6 | rename to `$'\e\a'` (all control) | **accepted**, stored as `^[^G` — the empty check ran before any strip |
| 7 | hostile `[session] name` in a template | rejection message replays the payload raw, **three times** |
| 8 | `--format json`, `events history` | safe (serde_json emits ``) |
| 9 | `TemplateError::NoPanes` | safe (`{:?}` → `\u{1b}`) |

### Root cause

- `Window::new` (`model.rs:142`) stores the title as given.
- `create_window` (`graph.rs:730`) has **no** title validation at all — not even the
  empty check `stage_create_window` has.
- `rename_window` (`graph.rs:877`) checks `is_empty()` on the **raw** string, then
  assigns it raw — so a control-only title survives as non-empty garbage (row 6).
- `stage_create_session` takes the template's `initial_window_title` unchecked.
- `crates/shux/src/cli.rs:3255` echoes the **client's raw `--name` argument** rather
  than the title the daemon actually stored, so even a fixed daemon would be replayed
  through the operator's terminal by the client.
- `GraphError::{InvalidSessionName, SessionNameTooLong, SessionNameExists,
  WindowNameConflict}` interpolate attacker-controlled text with `{0}` (Display, raw).
  These fire on input that is *rejected*, so it never reaches a sanitizer — row 7.

## Fix — two independent layers

### Layer 1 · ingress: one shared sanitizer, sanitize **then** validate

`sanitize_title()` becomes `pub` — the single title rule for panes *and* windows —
and its filter is widened from `char::is_control()` to a documented
`is_title_hostile()` covering the whole single-line invariant:

- **Cc** — C0, DEL, C1 (`char::is_control()`), the reported vector. C1 matters on its
  own: an 8-bit terminal reads `U+009B` as CSI and `U+009D` as OSC, with no ESC in sight.
- **U+2028 / U+2029** — line/paragraph separators; the border draw assumes one line.
- **U+202A–U+202E, U+2066–U+2069, U+200E, U+200F, U+061C** — bidi embedding, override,
  isolate and mark formatting: the Trojan-Source title-spoofing class (CVE-2021-42574),
  and the same set rustc's `text_direction_codepoint_in_literal` lint covers. Implicit
  bidi is untouched, so ordinary RTL titles still render. `U+200D` ZWJ is deliberately
  kept — it is load-bearing inside emoji sequences.

`Window::new` runs it, so *every* window construction site is covered by default, and so
do the auto-derived pane titles.

New `SessionGraph::validate_window_title(raw) -> Result<String, GraphError>` sanitizes
first and rejects an empty result with the existing `EmptyWindowName` — the ordering the
issue calls out. Wired into `create_window`, `stage_create_window`, `rename_window`
(conflict detection now compares the *sanitized* title) and `stage_create_session`'s
`initial_window_title`. Strip-not-reject, matching panes.

### Layer 2 · egress: nothing untrusted reaches the terminal unescaped

Rejected input never passes a sanitizer, so ingress alone cannot close row 7.

- The four free-text `GraphError` variants render through `str::escape_debug()`, so the
  payload shows as `\u{1b}` in the message the daemon returns and the CLI prints.
- `style::safe_label()` escapes control + separator + bidi characters and is applied
  inside every `print_*` helper and list renderer that interpolates a name, a title, a
  `cwd` or an argv. It is byte-identical for hostile-free input, so it is a pure backstop.
- `style::safe_diagnostic()` does the same for multi-line error text, keeping `\n` and
  `\t` so a three-line TOML diagnostic stays readable. This is what neutralizes the
  parse-error path, which runs before the daemon exists and which ingress sanitizing
  therefore cannot reach at all.
- `style::json_safe()` re-escapes a serialized JSON document's C1, separator and bidi
  characters to `\uXXXX` — `serde_json` escapes C0 and nothing above it. The
  pretty-printer's own newlines are structure and are left alone; escaping them would
  emit `\u000a` outside a string and produce invalid JSON.
- `handle_window_rename` prints the daemon's stored title instead of the raw argument —
  also more truthful, since the stored title is what the operator will see next.

Either layer alone stops the reported attack, and the egress layer is the only one that
can cover a value the daemon *rejected* — by definition it never met a sanitizer.

### Length: why windows diverge from panes

The first cut had window titles inherit the pane sanitizer's 64-char clamp. Adversarial
review killed that, and the reason generalises: **a window title is a lookup key.**
`window.ensure` is idempotent *by name* and `shux window … -w <name>` selects by it, so
silently truncating it makes two distinct requested names collapse onto one stored value.
Reproduced: `window rename -w "B×100-two"` renamed the `-one` window, and `window.ensure`
handed back the wrong window and pane. A pane title has no such role — panes are addressed
by ID — so panes keep clamping.

So `sanitize_title` strips without shortening (the genuinely shared rule),
`sanitize_title_clamped` adds the 64-char display clamp for panes, and over-long window
titles are **rejected** with `WindowNameTooLong` at 128 characters, mirroring
`SessionNameTooLong`. Loud beats silent when the value addresses something.

### Accepted behaviour changes

1. **Window titles over 128 characters are rejected** rather than stored. Previously
   unbounded. Bounded by rejection, not truncation — see above.
2. **`create_window` rejects an empty title.** It previously had no validation at all,
   while `stage_create_window` did, so the two paths disagreed. Whitespace-only titles are
   rejected too, since they trim to empty. A template's `title = ""` is **not** affected:
   the field is required, so a template with nothing to say writes `""`, and blank means
   "use the default name". A title that had content and sanitized away to nothing is the
   attack case and is still rejected.
3. **Titles are trimmed.** `--name "  padded  "` stores `padded`.
4. **Session-name length is now measured in characters**, matching what its error message
   always claimed. A 128-character non-ASCII name used to be refused by a byte limit.
5. **Error messages escape their payload** via `escape_debug`, which also escapes `"` and
   `\`. Non-ASCII is untouched.

## Adversarial review

Four agents driving the real binary on disjoint surfaces: template/apply ingress, CLI +
RPC, render/TUI, and a regression hunt A/B against a pre-fix build. Every finding was
independently reproduced here before being believed, and re-verified closed against the
same repro.

**Fixed (all confirmed):**

| # | Surface | Defect |
|---|---|---|
| P0 | template | TOML parse errors quote the offending source line verbatim, before the daemon exists — and a raw ESC in a template *is* a parse error, so the rejection replayed the payload. Also on `--dry-run`. |
| P0 | CLI | `pane list` printed `cwd` and `command` raw. Both are caller-supplied and legitimately arbitrary, so they are never sanitized on the way in. |
| P0 | TUI | The status bar painted the git branch name straight into cells. `git check-refname-format` forbids ASCII controls only, so C1 and bidi overrides survive into a ref. |
| P1 | core | Pane titles auto-derived from the command/cwd basename bypassed `sanitize_title` — so it was not "the single title rule" it claimed to be. |
| P1 | core | The clamp truncated a lookup key (above). |
| P1 | template | `title = ""` aborted the entire apply where it used to succeed. |
| P2 | CLI | `--format json` / `--dry-run` emitted raw C1, `U+2028/9` and bidi overrides — `serde_json` escapes C0 and nothing above it. |
| P2 | core | The bidi *marks* `U+200E`/`U+200F`/`U+061C` were missing from the hostile set. |
| P3 | CLI | `pane list`'s `command` column read a JSON array with `as_str()` and was permanently blank — and would have opened a second injection channel the moment someone fixed it alone. |
| P3 | core | `SessionNameTooLong` counted bytes while its message promised characters. |

**Reproduced, pre-existing, NOT fixed here** — none are caused or worsened by this change
relative to `main`, and each needs its own surface and test matrix:

- The status bar has no per-zone truncation, so a long center segment paints over the
  session identity and the detach hint. A 24-character title already overflows at 80
  columns, far below any clamp — the clamp never addressed this.
- The status bar and the compositor's pane-title overlay count characters where the
  layout counts display columns, so CJK and combining marks disagree between the live
  TUI and the rasterizer.
- `create_window` has no duplicate-title check while `rename_window` does. Verified with
  plain ASCII names on both the pre-fix and fixed builds — the asymmetry predates this
  change.
- Template `ratio` is unvalidated: negative, `1e30` and `0.0` all commit, and NaN/Inf
  surface as `invalid type: null, expected f32`.

## Testing matrix

| Level | Coverage |
|---|---|
| `shux-core` unit (`model.rs`) | ESC/OSC/BEL/C0/C1/DEL/newline/tab strip · bidi + separator strip · 64-char clamp · trim · control-only → empty · `Window::new` sanitizes · idempotence · sanitized output is a fixed point |
| `shux-core` unit (`graph.rs`) | `create_window` / `rename_window` / `stage_create_window` / `stage_create_session` sanitize and store clean · control-only → `EmptyWindowName` · conflict detection uses the sanitized title · `WindowCreated` / `WindowRenamed` events carry sanitized titles · `GraphError` Display escapes |
| `shux` unit (`style.rs`) | `safe_label` escapes each hostile class · no-op for clean input · every `print_*` helper emits no raw control byte |
| `shux` integration (real binary, daemon-backed) | Rows 1–7 above driven end-to-end against the real binary; every stdout/stderr byte scanned for C0/C1/DEL; JSON and text paths; `--format plain`; cross-path consistency (`window list` text ≡ json ≡ attach) |

## Acceptance criteria (from the issue)

- [x] A template, `window create`, or `window rename` carrying ESC, OSC, C0 and C1 bytes
      produces no active control sequence in any terminal-facing output.
- [x] A title that sanitizes to empty is rejected by the existing name validation rather
      than stored.
- [x] Pane and window titles go through one shared sanitizer — including the auto-derived
      pane titles that bypassed it until adversarial review.

## DoD

- [x] Every test above seen **failing first** (RED) against the unfixed tree.
- [x] `make check` green (clippy `-D warnings` + fmt + full nextest).
- [x] Adversarial review — four parallel agents driving the real binary; every finding
      reproduced here, fixed, and regression-tested.
- [x] Zero leaked daemons.
- [x] Visual proof for the fixed surfaces, published as a Claude Artifact and linked
      from the PR; no screenshots committed. The six "after" captures were re-taken
      against final HEAD and are byte-identical to the published set.
- [ ] `dootsabha` council — **unavailable in this cloud environment** (no CLI, no
      `~/.config/dootsabha/config.yaml`). Adversarial review stood in.
