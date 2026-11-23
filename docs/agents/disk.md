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
reason, then the total and the command to apply. With `APPLY=1` it runs `make reap`'s
script first, then removes the same plan. It exits non-zero if any removal fails, or if
a worktree's build dirs could not be listed.

| Setting | Default | Meaning |
|---|---|---|
| `PRUNE_CACHE_MIB` | `5120` | a cache bigger than this is removed whatever its age |
| `PRUNE_CACHE_DAYS` | `14` | a cache with nothing written for this long is removed |
| `PRUNE_SCRATCH_DAYS` | `7` | a `.shux/out/<entry>` with nothing written for this long is removed |
| `PRUNE_BUSY_MINUTES` | `30` | anything written this recently is in use, and kept |
| `PRUNE_BASE_REF` | `origin/main` | the branch a worktree counts as merged into |
| `PRUNE_TMP_ROOTS` | `/tmp $TMPDIR` | where runtime dirs are looked for |
| `SHUX_NEUTRALITY_CACHE` | `~/.cache/shux/vt-neutrality` | the vt-neutrality cache, as `make vt-neutrality` uses it |
| `SHUX_DISK_MIN_GIB` | `40` | `make disk-check`'s threshold |

Refs are read as they are locally: `git fetch --prune` first if remote branches moved.

## What it removes

Per git worktree of the repo (this checkout included), only when the worktree is
**idle**, see below:

- **build output**: the `target/` dirs `make clean` empties, as
  `scripts/check-target-hygiene.sh --list` names them (every Cargo workspace root's
  `target`, never a symlink or a configured `CARGO_TARGET_DIR`);
- **test-inventory cache** (`.shux/out/test-inventory`): over the size limit or unused
  for `PRUNE_CACHE_DAYS`;
- **stale scratch**: each `.shux/out/<entry>` unused for `PRUNE_SCRATCH_DAYS`.

Machine-wide:

- **vt-neutrality cache**: each `bin/<base>-<toolchain>`, `target` and `fuzz-target`
  unused for `PRUNE_CACHE_DAYS`, or all three when the cache is over the size limit;
- **runtime dirs** in `PRUNE_TMP_ROOTS`: a `shux*` dir of yours holding only `shux/`
  (and ble.sh's `blesh/`), or the fallback `shux-$UID`, whose files are only what a
  daemon writes (`shux*.pid`, `shux*.lock`, `*.sock`, `segment-*.toml`), and whose
  pidfiles and lockfiles name no live process.

## What it never removes

- anything in a worktree with uncommitted or untracked changes, or with commits on no
  remote that are not merged into `PRUNE_BASE_REF`; a squash-merged branch counts as
  merged when merging it into the base would change nothing;
- anything in a worktree with a running `cargo`/`rustc`/`rustdoc`/`clippy-driver`/`make`
  whose cwd is inside it (a build in a nested worktree holds only that one), or whose
  `target/` was written in the last `PRUNE_BUSY_MINUTES`; a locked or missing worktree;
- `.shux/qa`, and any `.shux/out` path git tracks;
- the vt-neutrality cache while its `lock` exists or it was written recently, and its
  `src` checkout, which `make vt-neutrality` recycles itself;
- a runtime dir with a live pid, written recently, holding anything else, a symlink, or
  owned by another user.

## Internals

`scripts/prune.sh` builds the whole plan first, then (with `--apply`) removes it entry by
entry through `safe_rm`, which refuses an empty path or root, `/`, `$HOME` as a root, a
relative path, `.`/`..`, a symlink, and any path that does not resolve strictly inside the
root it was planned under. A build's cwd comes from `/proc` on Linux and `lsof` on macOS
(`.shux/scripts/lib/proc_scope.sh`); with neither, every worktree is kept. Sizes are
`du -sk`, ages `find -mmin`/`-mtime`, free space `df -Pk`: the same flags on macOS and
Linux, and Bash 3.2 compatible.

`scripts/disk-check.sh --hook session` prints plain stdout, which Claude Code adds to the
agent's context; `--hook stop` prints `{"systemMessage": ...}`, shown to the user without
continuing the turn. Under `--hook` it always exits 0 (a Stop hook's exit 2 would block
the stop); a df it cannot read becomes the message.

`make check-prune` (in `make check` and CI on Linux and macOS Bash 3.2) runs
`scripts/disk-check-selftest.sh` against a stub `df`, and `scripts/prune-selftest.sh`,
which builds throwaway repos, worktrees, caches and runtime dirs and checks every
promise above; then it removes each protection from a copy of `prune.sh` in turn and
requires the suite to fail for that protection's reason.
