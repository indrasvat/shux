# 094 — `--cmd` promised a shell command and delivered a whitespace split

**Status:** Done
**Priority:** High (silent wrong execution; the pane starts, so nothing looks broken)
**Milestone:** M3 polish
**Depends On:** —
**Quality Gate:** shux-tui-qa — CLI/agent-workflow surface, no VT/raster change
**Touches:** `crates/shux/src/pane_command.rs` (new), `crates/shux/src/main.rs`,
`crates/shux/src/cli.rs`, `crates/shux/src/attach.rs`,
`crates/shux-core/src/graph.rs`, `crates/shux-core/src/model.rs`,
`crates/shux-pty/src/handle.rs`, `crates/shux/tests/pane_command_e2e.rs` (new),
`crates/shux/tests/window_title_escape_injection.rs`,
`.shux/scripts/issue_125_evidence.sh` (new)

---

## Problem (issue #125)

`--cmd` is documented as a **shell command**:

```
--cmd
        Shell command to run in the initial pane (single string)
```

`session.create` / `session.ensure` split that string on whitespace and exec'd the
words directly. Every shell construct — `;`, `|`, `&&`, quotes, globs, redirection,
`$VAR` — was handed to the first word as an ordinary argument. Nothing warned; the
pane started; the wrong thing ran.

```
$ shux session create a1 -d --cmd "printf 'X\n'; sleep 300"
$ shux pane capture -s a1
'X
';printf: warning: ignoring excess arguments, starting with 'sleep'
```

`printf` received `'X\n';`, `sleep` and `300` as three arguments. `printf` then
exited, so the pane's PTY was gone and the next `pane send-keys` failed with
`pane PTY '<id>' not found` — a second, unrelated-looking symptom of the same cause.

Quoting was lost the same way: `--cmd "echo 'hello world'"` printed `'hello world'`
with the quotes.

### The same surface, four more silent failures

Auditing every ingress for a pane command turned up four defects of the same class —
input accepted, wrong thing done, no error:

| # | Surface | Before |
|---|---|---|
| 1 | `session.create` / `session.ensure`, string `command` | split on whitespace |
| 2 | `window.create` / `window.ensure` / `pane.split`, string `command` | **ignored** — `.as_array()` misses, pane silently gets the default shell |
| 3 | all five, `command: ["vim", null]` | non-string elements silently **dropped**, `vim` runs bare |
| 4 | all five, `command: 42` / `{}` / `true` | silently **ignored** → default shell |
| 5 | `window.create` / `window.ensure` / `pane.split` | the command is exec'd but never persisted on the pane, so `pane list` shows a blank command column and the auto-title falls back to the cwd basename |

`window create --cmd` was already correct at the CLI level — it wrapped in
`sh -c` client-side — which is why the two verbs disagreed with each other as well as
with their own help text.

## The fix

One parser, `crates/shux/src/pane_command.rs`, owns the whole contract and is used by
all five handlers:

| `command` | meaning |
|---|---|
| `["nvim", "a b.rs"]` | argv, exec'd directly, no shell, no splitting |
| `"printf 'X\n'; sleep 300"` | a **shell** command — run as `$SHELL -c <string>` (`/bin/sh` when `$SHELL` is unset) |
| omitted / `null` / `""` / `[]` | the user's default login+interactive shell (unchanged) |
| `42`, `{}`, `true`, `["vim", null]`, `[""]`, anything with a NUL | `invalid_params` naming the offending value |

`$SHELL` is the same shell a bare pane already gets (`PtyConfig::resolve_command`
spawns `$SHELL -l -i`), so `--cmd "source .venv/bin/activate && python app.py"` behaves
the way the same line does in a pane the user opened by hand.

The CLI keeps sending `--cmd` as a JSON string and stops doing its own `sh -c` wrap in
`window create`, so CLI and RPC agree — one contract, one implementation, no
client-side transformation the API doesn't perform.

### Titles keep naming the program, not the shell

Wrapping in `$SHELL -c` moves `command[0]` from `top` to `/bin/bash`, which would have
retitled every `--cmd` pane after its shell. `Pane::recalculate_title` now unwraps a
shell wrapper (`<shell> -c|-lc|-ic|-lic <script>`) and derives the title from the first
real word of the script, skipping `exec` and `NAME=value` prefixes and falling back to
the shell's own name when the script does not start with a plain command word. The
long-documented escape hatch `-- sh -c "npm run dev"` gains the same benefit: it used
to title the pane `sh`, now `npm`.

### Follow-ups from adversarial review

Three parallel agents drove the real binary on disjoint surfaces (shell semantics,
the RPC contract, titles + rich TUIs). Each finding was reproduced before it was
believed and fixed with a regression test. All are the same shape as the issue.

| # | Surface | Before |
|---|---|---|
| 6 | `window.ensure` on an **existing** window | parsed `command` after the already-exists shortcut, so every malformed shape was accepted in exactly the case the verb is named for |
| 7 | all five spawning RPCs | a PTY that never started returned **success**: `✓ Created session` over a pane that answered "pane VT not found" to everything afterwards. Now `SPAWN_FAILED` + rollback |
| 8 | `session attach` | respawned the active pane whenever no PTY was registered, sweeping up panes that had genuinely **exited** — a pane that ran `make` and finished came back as a login shell in the daemon's cwd |
| 9 | `PtyConfig::resolve_command` | `env::var` returns `Ok("")` for a set-but-empty `$SHELL`, so `unwrap_or_else` never fired: `SHELL=""` gave a working `--cmd` pane and a **dead** default pane from one daemon |
| 10 | argv validation | `["   "]` accepted where `[""]` was rejected; both exec a program name that cannot resolve |
| 11 | argv validation | an argument past `MAX_ARG_STRLEN` accepted, then `E2BIG` at `execve` with no diagnosis |
| 12 | `state.apply` | a **sixth** spawner. Typed ops, so serde proved `Vec<String>` and nothing proved the strings could reach `execve` |
| 13 | titles (introduced by this task) | `A=1;htop -d 10` titled `-d` — the scanner skipped a complete leading assignment and walked into a flag belonging to a command it never established. `if`/`for`/`while` became titles, contradicting the function's own doc |
| 14 | `pane split` CLI | `pane.split` has always accepted `command`; the CLI had no way to say it |
| 15 | `--cmd` flag parsing | no `allow_hyphen_values`, so a flag-shaped command was refused — and clap's tip pointed at `--`, which is the argv form, a different execution model |

Title derivation now reads **only the first simple command** (everything before the
first shell operator), treats shell keywords as "no single program to name", and
splits on operators so `ls|wc` still reads as `ls`.

### Round two — what the first cut of the follow-ups got wrong

A second pair of agents attacked the fixes above. Every finding reproduced
against the real binary before it was believed; two were regressions this task
introduced.

| # | Surface | Before |
|---|---|---|
| 16 | rollback on a failed `window.create` / `window.ensure` | **the session's active window moved.** Creating a window focuses it; destroying one hands focus to the session's *first* window, not the one that had it. `three` → `1`, and every later `-w`-less verb then targeted the wrong window — including a fresh attach |
| 17 | rollback on a failed `pane.split` | same shape one level down: the active pane moved. Under concurrency this also made *healthy* splits fail with `pane 'X' not found`, because a concurrent caller resolving "the active pane" latched onto the id about to be destroyed |
| 18 | `MAX_ARG_BYTES` | off by one. `MAX_ARG_STRLEN` counts the terminating NUL, so 131071 is the longest argument that fits and 131072 is not — the cap was 131072, and **the unit test asserted that length was fine, and was green.** A test never seen failing against the real system |
| 19 | argv total size | the per-argument cap did not bound the sum: 40 × 100 KiB is 4 MiB and still `E2BIG` at `execve`, with a hint pointing at `argv[0]` and the cwd, neither of which was wrong |
| 20 | pane titles | a flag is not a program. `--cmd "-n is a valid sed script"` — the example in the flag's own help — titled the pane `-n`; so did `A=1 -d 10`. `/`, `..`, `+++` and `--` became titles verbatim because `basename` returns the whole token when there is no file name in it |
| 21 | `state apply --dry-run` | did not validate. The argv rule lived only in the daemon, so the flag whose entire purpose is "will this apply succeed?" answered yes to templates the real run rejects |
| 22 | `state apply` | printed `✓ Applied` and exited 0 when every pane in the batch failed to spawn, so `shux state apply t.toml && shux attach` walked into a session of dead panes |
| 23 | title sanitizer | `U+200B` and friends survived, so `ht<ZWSP>op` renders identically to `htop` — the same spoofing shape as the bidi set, without needing any reordering |

**Known residual, deliberately not fixed here.** Between the split and the spawn
failure the new pane *is* the window's active pane, so a concurrent caller
resolving "the active pane" in that window can still be handed an id that is
about to be destroyed, and gets `pane not found`. Closing it means not focusing
the new pane until the spawn succeeds, which adds two focus events to every
successful split. Before this task the same caller got a *phantom* pane that
resolved and was dead, so this is strictly an improvement: a loud error instead
of a silent wrong pane.

**Also unchanged, by design.** `state.apply` still does not roll back a partial
batch (codex P0 #1: killing already-spawned siblings has its own side effects,
so partial outcomes are reported rather than undone). What changed is that the
report is no longer a green tick and exit 0.

## Testing Matrix

| Level | Coverage |
|---|---|
| Unit — `pane_command.rs` | argv passthrough; shell wrapping; blank/empty/null → default shell; every rejection (number, bool, object, non-string element, empty argv[0], NUL, blank string element in argv) with message assertions; `$SHELL` unset/blank fallback |
| Unit — `model.rs` | shell-wrapper title unwrapping: plain argv, `-c`/`-lc`/`-ic`/`-lic`, `exec` prefix, `FOO=1` prefix, metacharacter first word → shell fallback, non-shell program named `-c`, 2-element wrapper, sanitization still applied |
| Unit — `graph.rs` | `create_window_with_command` / `split_pane_with_command` persist `Pane.command` and put it in the `PaneCreated` event |
| Unit — `cli.rs` | `session create` / `window create` param building: `--cmd` goes out as a string, trailing argv as an array, argv wins |
| E2E — `tests/pane_command_e2e.rs` | real daemon, real PTYs: `;`, `|`, `&&`, `||`, quotes, glob, redirect+read-back, `$VAR`, subshell, heredoc, multi-line, unicode, exit code, argv-with-spaces passthrough, `--cmd ""`, all five RPCs, every rejection over the wire, `pane list` truthfulness, auto-title |
| E2E — colour | every capture case includes truecolor + 256-indexed + basic SGR so a monochrome regression cannot pass |
| Unit — `attach.rs` | attach spawns a pane that never ran, never one that exited (every exit status) |
| Unit — `handle.rs` | blank/whitespace/unset `$SHELL` all fall back; an explicit command is never replaced |
| E2E — follow-ups | `window.ensure` on an existing window; blank argv[0]; oversize argument; a program that cannot be executed on all five verbs, with no phantom left behind; `state.apply` argv validation; `pane split --cmd` and trailing argv; a hyphen-leading `--cmd`; a blank `$SHELL` still opening a working default pane |
| E2E — round two | focus survives a failed `window.create` and a failed `pane.split`; the argument-length boundary pinned against a **real spawn** on both sides (131071 spawns, 131072 refused before `execve`); an argv whose total is too long; `--dry-run` and the real run agreeing; `state apply` reporting failure; a flag-shaped `--cmd` never becoming a title. All seven observed failing against `dcb4a1e` and passing here |
| Shell — `.shux/scripts/issue_125_evidence.sh` | eight scenes, both binaries, through the shipped binary, under the leak guard. Colour is asserted on the **pen** via `pane glance --cells`, not on the word: the pre-fix screen prints the literal text `INDEXED` inside printf's own error message, so a `grep` would have called that a colour probe. `EXPECT_DEFECT=1` inverts the verdict, so the baseline arm fails if the defect has already gone away |

## Acceptance Criteria

- [x] The issue's two reproductions produce the documented result.
- [x] A string `command` means the same thing on all five RPCs.
- [x] Malformed `command` is an error naming the value, never a silent default shell.
- [x] `window.create` / `window.ensure` / `pane.split` persist the command they ran.
- [x] `--cmd top` still titles the pane `top`.
- [x] Trailing `-- argv...` is unchanged: exec'd directly, no shell.
- [x] A `command` that cannot be executed is an error, not a session.
- [x] Attaching never silently replaces a pane's program.
- [x] A rolled-back create leaves focus exactly where it was.
- [x] The argument-length limit is the one `execve` actually enforces, pinned by a real spawn.
- [x] Zero leaked daemons or child processes.

## DoD

- [x] RED test observed failing for every defect above before the fix.
- [x] `make check` green.
- [x] Adversarial review driving the real binary; findings reproduced and fixed.
- [x] Evidence script + visual proof in the PR.
