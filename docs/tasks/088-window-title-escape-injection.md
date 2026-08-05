# 088 — window titles bypass `sanitize_title()` and reach the terminal with control bytes intact

**Status:** In Progress
**Priority:** High (security — terminal escape injection via untrusted template / RPC input; issue #104)
**Milestone:** M3 polish
**Depends On:** 027 (pane titles), 033 (templates / `state apply`)
**Touches:** `crates/shux-core/src/model.rs`, `crates/shux-core/src/graph.rs`,
`crates/shux/src/style.rs`, `crates/shux/src/cli.rs`,
`crates/shux/tests/window_title_escape_injection.rs` (new)

---

## Problem (issue #104)

Window titles are stored raw and printed raw to the operator's terminal, so a title
carrying escape sequences **executes them**. `shux state apply` against a template from
an untrusted repo is the realistic vector: TOML forbids raw control bytes in a basic
string, but its own `\uXXXX` escapes decode to real control bytes before shux ever sees
them.

`sanitize_title()` (`model.rs:342`) — strip `char::is_control()`, trim, clamp to 64 —
already exists and is wired **only** to the pane paths (`model.rs:268` manual title,
`model.rs:278` OSC title). Every window path bypasses it.

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

- **Cc** — C0, DEL, C1 (`char::is_control()`), the reported vector;
- **U+2028 / U+2029** — line/paragraph separators; the border draw assumes one line;
- **U+202A–U+202E, U+2066–U+2069** — bidi embedding/override/isolate formatting, the
  Trojan-Source title-spoofing class (CVE-2021-42574). Implicit bidi is untouched, so
  ordinary RTL titles still render.

`Window::new` runs it too, so *every* construction site is covered by default.

New `SessionGraph::validate_window_title(raw) -> Result<String, GraphError>` sanitizes
first and rejects an empty result with the existing `EmptyWindowName` — the ordering the
issue calls out. Wired into `create_window`, `stage_create_window`, `rename_window`
(conflict detection now compares the *sanitized* title) and `stage_create_session`'s
`initial_window_title`. Strip-not-reject, matching panes.

### Layer 2 · egress: nothing untrusted reaches the terminal unescaped

Rejected input never passes a sanitizer, so ingress alone cannot close row 7.

- The four free-text `GraphError` variants render through `str::escape_debug()`, so the
  payload shows as `\u{1b}` in the message the daemon returns and the CLI prints.
- `style::safe_label()` escapes control + separator + bidi-override characters and is
  applied inside every `print_*` helper that interpolates an entity name or title. It is
  byte-identical for hostile-free input, so it is a pure backstop.
- `handle_window_rename` prints the daemon's stored title instead of the raw argument —
  also more truthful, since the stored title is what the operator will see next.

Either layer alone stops the attack; both together mean a future unsanitized path
cannot become an injection.

## Testing matrix

| Level | Coverage |
|---|---|
| `shux-core` unit (`model.rs`) | ESC/OSC/BEL/C0/C1/DEL/newline/tab strip · bidi + separator strip · 64-char clamp · trim · control-only → empty · `Window::new` sanitizes · idempotence · sanitized output is a fixed point |
| `shux-core` unit (`graph.rs`) | `create_window` / `rename_window` / `stage_create_window` / `stage_create_session` sanitize and store clean · control-only → `EmptyWindowName` · conflict detection uses the sanitized title · `WindowCreated` / `WindowRenamed` events carry sanitized titles · `GraphError` Display escapes |
| `shux` unit (`style.rs`) | `safe_label` escapes each hostile class · no-op for clean input · every `print_*` helper emits no raw control byte |
| `shux` integration (real binary, daemon-backed) | Rows 1–7 above driven end-to-end against the real binary; every stdout/stderr byte scanned for C0/C1/DEL; JSON and text paths; `--format plain`; cross-path consistency (`window list` text ≡ json ≡ attach) |

## Acceptance criteria (from the issue)

- [ ] A template, `window create`, or `window rename` carrying ESC, OSC, C0 and C1 bytes
      produces no active control sequence in any terminal-facing output.
- [ ] A title that sanitizes to empty is rejected by the existing name validation rather
      than stored.
- [ ] Pane and window titles go through one shared sanitizer.

## DoD

- [ ] Every test above seen **failing first** (RED) against the unfixed tree.
- [ ] `make check` green (clippy `-D warnings` + fmt + full nextest).
- [ ] Adversarial review — parallel agents driving the real binary; findings reproduced,
      fixed, regression-tested.
- [ ] Zero leaked daemons.
- [ ] Visual proof for the fixed surfaces, published as a Claude Artifact and linked
      from the PR; no screenshots committed.
