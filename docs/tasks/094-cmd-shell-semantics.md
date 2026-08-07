# 094 — `--cmd` promised a shell command and delivered a whitespace split

**Status:** In Progress
**Priority:** High (silent wrong execution; the pane starts, so nothing looks broken)
**Milestone:** M3 polish
**Depends On:** —
**Quality Gate:** shux-tui-qa — CLI/agent-workflow surface, no VT/raster change
**Touches:** `crates/shux/src/pane_command.rs` (new), `crates/shux/src/main.rs`,
`crates/shux/src/cli.rs`, `crates/shux-core/src/graph.rs`,
`crates/shux-core/src/model.rs`, `crates/shux/tests/pane_command_e2e.rs` (new),
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

## Testing Matrix

| Level | Coverage |
|---|---|
| Unit — `pane_command.rs` | argv passthrough; shell wrapping; blank/empty/null → default shell; every rejection (number, bool, object, non-string element, empty argv[0], NUL, blank string element in argv) with message assertions; `$SHELL` unset/blank fallback |
| Unit — `model.rs` | shell-wrapper title unwrapping: plain argv, `-c`/`-lc`/`-ic`/`-lic`, `exec` prefix, `FOO=1` prefix, metacharacter first word → shell fallback, non-shell program named `-c`, 2-element wrapper, sanitization still applied |
| Unit — `graph.rs` | `create_window_with_command` / `split_pane_with_command` persist `Pane.command` and put it in the `PaneCreated` event |
| Unit — `cli.rs` | `session create` / `window create` param building: `--cmd` goes out as a string, trailing argv as an array, argv wins |
| E2E — `tests/pane_command_e2e.rs` | real daemon, real PTYs: `;`, `|`, `&&`, `||`, quotes, glob, redirect+read-back, `$VAR`, subshell, heredoc, multi-line, unicode, exit code, argv-with-spaces passthrough, `--cmd ""`, all five RPCs, every rejection over the wire, `pane list` truthfulness, auto-title |
| E2E — colour | every capture case includes truecolor + 256-indexed + basic SGR so a monochrome regression cannot pass |
| Shell — `.shux/scripts/issue_125_evidence.sh` | the issue's own reproduction, before/after, through the shipped binary, under the leak guard |

## Acceptance Criteria

- [x] The issue's two reproductions produce the documented result.
- [x] A string `command` means the same thing on all five RPCs.
- [x] Malformed `command` is an error naming the value, never a silent default shell.
- [x] `window.create` / `window.ensure` / `pane.split` persist the command they ran.
- [x] `--cmd top` still titles the pane `top`.
- [x] Trailing `-- argv...` is unchanged: exec'd directly, no shell.
- [x] Zero leaked daemons or child processes.

## DoD

- [x] RED test observed failing for every defect above before the fix.
- [x] `make check` green.
- [x] Adversarial review driving the real binary; findings reproduced and fixed.
- [x] Evidence script + visual proof in the PR.
