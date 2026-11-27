#!/usr/bin/env bash
# .shux/scripts/flake_gate_replay.sh — replay merged PRs through the old and
# the new flake gate, and record what each selects.
#
# For each squash merge on main (each against its parent) it runs both gates'
# selection on a scratch worktree of that commit, lists each filter's tests
# with `cargo nextest list`, and checks:
#   after holds every changed test   a test whose fn a hunk is in (git's rust
#                                    diff driver names it) or whose fn a hunk adds
#   after ⊆ before                   reported when not: the new gate counts more
#                                    as test code (any cfg naming test, not(test)
#                                    included), which can only add tests
# A PR that drops a changed test fails the run; every PR stays in the data.
#
#   $@                    commits to replay (default: the last FLAKE_REPLAY_COUNT
#                         squash merges on origin/main, release commits aside)
#   FLAKE_REPLAY_COUNT    default 20
#   FLAKE_REPLAY_TIME     commits whose 20 gated runs to time as well, both gates
#   FLAKE_PROOF_BEFORE    the old gate's commit (default 8ef34f8)
#   FLAKE_REPLAY_OUT      default .shux/out/flake-gate-replay (prs.jsonl + sets)
#   FLAKE_REPLAY_RECHECK  1: rerun only the checks, on the sets and times a
#                         previous run saved there (no builds)

set -euo pipefail

root="$(git rev-parse --show-toplevel)"
before_ref="${FLAKE_PROOF_BEFORE:-8ef34f8}"
out="${FLAKE_REPLAY_OUT:-$root/.shux/out/flake-gate-replay}"
recheck="${FLAKE_REPLAY_RECHECK:-}"
command -v ast-grep >/dev/null || { echo "✗ no ast-grep on PATH" >&2; exit 1; }
mkdir -p "$out"
previous=""
if [[ -n "$recheck" ]]; then
    previous="$(cat "$out/prs.jsonl" 2>/dev/null || true)"
fi
: >"$out/prs.jsonl"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$root/target}"

if (($# == 0)); then
    mapfile -t commits < <(git -C "$root" log --format='%h %s' origin/main |
        grep -v 'chore(release)' | grep -E '\(#[0-9]+\)$' | head -n "${FLAKE_REPLAY_COUNT:-20}" | cut -d' ' -f1)
else
    commits=("$@")
fi

scratch="$(mktemp -d "${TMPDIR:-/tmp}/shux-flake-replay.XXXXXX")"
wt="$scratch/wt"
git -C "$root" show "$before_ref:scripts/flake-gate.sh" >"$scratch/gate-before.sh"
cp "$root/scripts/flake-gate.sh" "$scratch/gate-after.sh"
cp "$root/lint/ast-grep/test-spans.yml" "$scratch/test-spans.yml"
cp "$root/scripts/test-only-files.sh" "$scratch/test-only-files.sh"
cleanup() {
    git -C "$root" worktree remove --force "$wt" 2>/dev/null || true
    rm -rf "$scratch"
}
trap cleanup EXIT
git -C "$root" worktree add -q --detach "$wt" "${commits[0]}"
printf '*.rs diff=rust\n' >"$scratch/attributes"
# shellcheck disable=SC2016  # expands when the stub runs
# One line per tier the gate runs; what it selects is their union.
printf '#!/usr/bin/env bash\nprintf "%%s\\n" "$STRESS_FILTER" >>"$FLAKE_PROOF_FILTER"\n' >"$scratch/stress-stub.sh"

# install <side>: that gate, and the new gate's own files, into the worktree.
install() {
    git -C "$wt" checkout -q -- . 2>/dev/null || true
    git -C "$wt" clean -qfd -- lint scripts
    mkdir -p "$wt/lint/ast-grep"
    cp "$scratch/gate-$1.sh" "$wt/scripts/flake-gate.sh"
    cp "$scratch/test-spans.yml" "$wt/lint/ast-grep/test-spans.yml"
    cp "$scratch/test-only-files.sh" "$wt/scripts/test-only-files.sh"
}

# pick <side>: the tests that gate selects, into $scratch/<side>.set.
pick() {
    local filter
    install "$1"
    cp "$scratch/stress-stub.sh" "$wt/scripts/test-stress.sh"
    : >"$scratch/filter"
    (cd "$wt" && BASE=HEAD~1 FLAKE_PROOF_FILTER="$scratch/filter" bash scripts/flake-gate.sh) >"$scratch/$1.log" 2>&1
    filter="$(awk 'NF { printf "%s(%s)", (n++ ? " + " : ""), $0 }' "$scratch/filter")"
    if [[ -z "$filter" ]]; then
        : >"$scratch/$1.set"
    else
        (cd "$wt" && cargo nextest list --color never --workspace --ignore-default-filter -E "$filter" --message-format oneline) | sort -u >"$scratch/$1.set"
    fi
}

# timed <side>: seconds for that gate's real 20 runs under load.
timed() {
    local start
    install "$1"
    start="$(date +%s)"
    (cd "$wt" && BASE=HEAD~1 bash scripts/flake-gate.sh) >&2
    echo $(($(date +%s) - start))
}

# changed_tests: names of the fns the worktree's HEAD commit adds or edits,
# found without the gate's own machinery: a line that adds `fn <name>`, or a
# hunk git's rust diff driver labels `fn <name>` that starts before that fn's
# closing brace (counted by braces), so not one merely placed after it.
changed_tests() {
    changed_unsorted | sort -u
}
changed_unsorted() {
    local path start name end
    git -C "$wt" diff -U0 HEAD~1 HEAD -- 'crates/*.rs' |
        awk '/^\+[^+]/ && match($0, /fn [A-Za-z0-9_]+/) { print substr($0, RSTART + 3, RLENGTH - 3) }'
    git -C "$wt" -c core.attributesFile="$scratch/attributes" diff -U0 HEAD~1 HEAD -- 'crates/*.rs' |
        awk '/^\+\+\+ b\// { path = substr($0, 7) }
             /^@@/ && match($0, /fn [A-Za-z0-9_]+/) {
                 name = substr($0, RSTART + 3, RLENGTH - 3)
                 split($3, r, ","); print path "\t" substr(r[1], 2) "\t" name
             }' |
        while IFS=$'\t' read -r path start name; do
            end="$(git -C "$wt" show "HEAD:$path" | awk -v n="$name" '
                !on && $0 ~ ("fn " n "[^A-Za-z0-9_]") { on = 1 }
                on && !done { o = gsub(/\{/, "{"); d += o - gsub(/\}/, "}"); if (o) opened = 1; if (opened && d <= 0) { print NR; done = 1 } }')"
            if [[ -n "$end" ]] && ((start <= end)); then echo "$name"; fi
        done
}

fail=0
for c in "${commits[@]}"; do
    git -C "$wt" checkout -q -- . 2>/dev/null || true
    git -C "$wt" clean -qfd -- lint scripts
    git -C "$wt" checkout -q --detach "$c"
    pr="$(git -C "$root" log -1 --format=%s "$c" | grep -oE '\(#[0-9]+\)$' | tr -d '(#)')"
    printf '\033[1;36m▶ #%s (%s)\033[0m\n' "$pr" "$c"
    if [[ -n "$recheck" ]]; then
        cp "$out/$pr.before" "$scratch/before.set"
        cp "$out/$pr.after" "$scratch/after.set"
    else
        pick before
        pick after
        cp "$scratch/before.set" "$out/$pr.before"
        cp "$scratch/after.set" "$out/$pr.after"
    fi
    changed_tests >"$scratch/changed"
    awk 'NR == FNR { want[$0] = 1; next } { n = $2; sub(/.*::/, "", n); if (n in want) print }' \
        "$scratch/changed" "$scratch/before.set" >"$scratch/must"
    missing="$(comm -23 "$scratch/must" "$scratch/after.set")"
    outside="$(comm -13 "$scratch/before.set" "$scratch/after.set")"
    b="$(grep -c . "$scratch/before.set" || true)"
    a="$(grep -c . "$scratch/after.set" || true)"
    m="$(grep -c . "$scratch/must" || true)"
    bs=null as=null
    if [[ -n "$recheck" ]]; then
        bs="$(jq -r --arg c "$c" 'select(.commit == $c) | .before.secs' <<<"$previous")"
        as="$(jq -r --arg c "$c" 'select(.commit == $c) | .after.secs' <<<"$previous")"
        bs="${bs:-null}" as="${as:-null}"
    elif [[ " ${FLAKE_REPLAY_TIME:-} " == *" $c "* ]]; then
        bs="$(timed before)"
        as="$(timed after)"
    fi
    ok=true
    [[ -z "$missing" ]] || { ok=false; fail=1; }
    printf '{"pr":%s,"commit":"%s","before":{"tests":%d,"secs":%s},"after":{"tests":%d,"secs":%s},"changed_tests":%d,"missing":%d,"outside":%d,"ok":%s}\n' \
        "$pr" "$c" "$b" "$bs" "$a" "$as" "$m" "$(grep -c . <<<"$missing" || true)" "$(grep -c . <<<"$outside" || true)" "$ok" \
        >>"$out/prs.jsonl"
    if [[ "$ok" == true ]]; then
        printf '\033[1;32m  ✓ before %d → after %d (keeps all %d changed tests)\033[0m\n' "$b" "$a" "$m"
    else
        printf '\033[1;31m  ✗ before %d → after %d\033[0m\n' "$b" "$a"
        printf '\033[31m    drops: %s\033[0m\n' "$missing"
    fi
    [[ -z "$outside" ]] || printf '\033[33m    ⚠ also selects, beyond before: %s\033[0m\n' "$outside"
done
if ((fail)); then
    printf '\033[1;31m✗ flake gate replay: some PRs break a check (%s)\033[0m\n' "$out/prs.jsonl"
    exit 1
fi
printf '\033[1;32m✓ flake gate replay: %d PRs, every check holds (%s)\033[0m\n' "${#commits[@]}" "$out/prs.jsonl"
