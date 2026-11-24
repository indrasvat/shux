# Disk: `make prune` and `make disk-check`

## TL;DR

```bash
make prune            # dry run: what would go, how much, and why the rest stays
make prune APPLY=1    # remove it
make disk-check       # silent above 40 GiB free; one line naming make prune below it
```

Run `make prune APPLY=1` after a PR's post-merge smoke. Claude Code's SessionStart and
Stop hooks run `make disk-check`, so a low disk is reported without anyone remembering.

## Usage

`make prune` changes nothing unless `APPLY=1`. It prints every candidate as `- remove`
with its size and the reason, everything it looked at and left as `= keep` with the
reason, then the total and the command to apply. With `APPLY=1` each entry is checked
again just before it is removed, and kept if it no longer qualifies. It exits non-zero if
any removal fails, or if a worktree's build dirs could not be listed.

It never stops a daemon or a session. Daemons this checkout started are listed (as
`make reap --dry-run` would); if they are leaked, stop them as a separate step with
`make reap`.

| Setting | Default | Meaning |
|---|---|---|
| `PRUNE_CACHE_MIB` | `5120` | a cache bigger than this is removed whatever its age |
| `PRUNE_CACHE_DAYS` | `14` | a cache with nothing written for this long is removed |
| `PRUNE_SCRATCH_DAYS` | `7` | a `.shux/out/<entry>` with nothing written for this long is removed |
| `PRUNE_TMP_ROOTS` | `/tmp:$TMPDIR` | colon-separated absolute dirs to look for runtime dirs in |
| `SHUX_NEUTRALITY_CACHE` | `~/.cache/shux/vt-neutrality` | the vt-neutrality cache, as `make vt-neutrality` uses it |
| `SHUX_DISK_MIN_GIB` | `40` | `make disk-check`'s threshold, 0 to 999999, no leading zeros |

Fixed: anything written in the last 30 minutes is in use, and a worktree counts as merged
into `origin/main`. Refs are read as they are locally: `git fetch --prune` first if
remote branches moved.

## What it removes

Per git worktree of the repo (this checkout included), only when the worktree is
**idle** (see below):

- **build output**: the `target/` dirs `make clean` empties, as
  `scripts/check-target-hygiene.sh --list` names them (every Cargo workspace root's
  `target`, never a symlink or a configured `CARGO_TARGET_DIR`);
- **test-inventory cache** (`.shux/out/test-inventory`): over the size limit or unused
  for `PRUNE_CACHE_DAYS`;
- **stale scratch**: each `.shux/out/<entry>` unused for `PRUNE_SCRATCH_DAYS`.

Machine-wide:

- **vt-neutrality cache**: each `bin/<base>-<toolchain>`, `target` and `fuzz-target`
  unused for `PRUNE_CACHE_DAYS`, or all of them when the cache is over the size limit;
- **runtime dirs** in `PRUNE_TMP_ROOTS`: a `shux*` dir of yours holding only `shux/`
  (and ble.sh's `blesh/`), or the fallback `shux-$UID`, whose files are only what a
  daemon writes (`shux*.pid`, `shux*.lock`, `*.sock`, `segment-*.toml`), and whose
  pidfiles and lockfiles all name a pid that is not running.

## What it never removes

- anything in a worktree with uncommitted or untracked changes (submodules included,
  whatever `.gitmodules` says), or with commits on no remote that are not merged into
  `origin/main`; a squash-merged branch counts as merged when merging it into the base
  would change nothing;
- anything in a worktree with a running `cargo`/`rustc`/`rustdoc`/`clippy-driver`/`make`
  whose cwd is inside it (a submodule counts as inside it; a nested worktree holds only
  itself), or any of whose build dirs was written in the last 30 minutes; a locked or
  missing worktree, or one git lists at a path that is a symlink;
- a worktree's build dirs while a live shux daemon runs a binary built there;
- `.shux/qa`, anything git tracks, or anything git cannot say it does not track;
- anything reached through a symlink (`.shux`, `.shux/out`, a cache, a worktree, any
  directory on the way);
- the vt-neutrality cache while its `lock` exists or a marker in its `readers/` names a
  running pid (or cannot be read), and its `src` checkout;
- a runtime dir with a running pid in it (even one a new process reused), an unreadable
  or malformed pidfile, anything else in it, written recently, a symlink, or owned by
  another user.

## Internals

`scripts/prune.sh` builds the whole plan, then (with `--apply`) re-checks and removes it
entry by entry through `safe_rm`, which refuses anything not absolute, a root that is not
a physical directory or is `$HOME`, and any path not strictly inside its root through
real directories (no symlink, `.`, `..` or empty component). Paths are never word-split:
worktrees come from `git worktree list --porcelain -z`, build dirs from
`check-target-hygiene.sh --list` (NUL-terminated), entries from `find -print0`, and path
resolution keeps trailing newlines. A build's cwd comes from `/proc` on Linux and `lsof`
on macOS (`.shux/scripts/lib/proc_scope.sh`); if one cannot be seen, every worktree is
kept. Sizes are `du -sk`, ages `find -mmin`/`-mtime`, free space `df -Pk`: the same flags
on macOS and Linux, Bash 3.2 compatible.

vt-neutrality and prune share the cache's `lock` directory. `scripts/vt-neutrality.sh`
takes it to write its `readers/<pid>` marker (and keeps it while it builds the base);
prune takes it before removing anything there and leaves the cache alone while a marker
names a running pid. A vt-neutrality run that finds the lock held waits up to 30 s.

`scripts/disk-check.sh --hook session` prints plain stdout, which Claude Code adds to the
agent's context; `--hook stop` prints `{"systemMessage": ...}`, shown to the user without
continuing the turn, with control characters replaced. Under `--hook` it always exits 0;
a df it cannot read or a malformed threshold becomes the message. The hook commands in
`.claude/settings.json` end in `|| echo …`, so a missing target or a broken Makefile
(make exits 2, which would block a Stop) still exits 0 with a message.

`make check-prune` (in `make check` and in CI on Linux and on macOS Bash 3.2) runs
`scripts/disk-check-selftest.sh` against a stub `df` and the exact hook commands, and
`scripts/prune-selftest.sh`, which builds throwaway repos, worktrees, submodules, caches
and runtime dirs and checks every promise above, a plan gone stale included; then it
takes each protection out of a copy of `prune.sh` in turn and requires the suite to fail
for that protection's reason.
