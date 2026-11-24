#!/usr/bin/env bash
# scripts/vt-neutrality.sh — prove a diff moves no VT cell or pixel, deterministically.
#
#   BASE=<sha> [SCOPE=<scope>] scripts/vt-neutrality.sh      (via `make vt-neutrality`)
#
# Layers, each compared at exact 0/0 with .claude/automations/pixel_verify.py:
#   corpus-goldens  HEAD's corpus harness against the committed goldens
#   corpus-ab       base and HEAD harnesses on base's fixtures (cells, replies, pixels)
#   live-ab         base and HEAD binaries replaying the committed rich-TUI recordings
#                   in real panes: pane.snapshot, window.snapshot and capture text
#   colour-probe    truecolor + indexed + basic in real panes at 80x24/120x40/200x60,
#                   asserted non-blank on both sides, then A/B'd the same way
#   fuzz            the VT-facing libFuzzer targets for NEUTRALITY_FUZZ_SECONDS each
#   leaks           every daemon this script started is gone
#
# The base build is cached outside target/, keyed on the base SHA and the toolchain
# base's rust-toolchain.toml resolves to. Override the location with
# SHUX_NEUTRALITY_CACHE.
#
# Exit: 0 neutral · 3 a cell or pixel moved (every layer ran) · 1 a layer could
# not run, crashed, leaked or proved nothing · 2 bad input. Through make every
# failure is 2, so callers read `status` (pass | moved | error) in the report.
# The report lands in .shux/out/vt-neutrality/neutrality.json, and with SCOPE also
# in .shux/qa/<scope>/neutrality.json, which scripts/check-vt-qa.sh reads.

set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"

die_input() { printf '\033[1;31m✗ %s\033[0m\n' "$1" >&2; exit 2; }
step() { printf '\033[1;36m▶ [+%ss] %s\033[0m\n' "$SECONDS" "$1"; }

base_ref="${BASE:-}"
[[ -n "$base_ref" ]] || die_input "BASE is empty: pass BASE=<sha> (the merge-base of this branch)"
base="$(git rev-parse --verify --quiet "${base_ref}^{commit}")" ||
    die_input "BASE='${base_ref}' does not resolve to a commit"
head="$(git rev-parse HEAD)"
scope="${SCOPE:-}"
if [[ -n "$scope" && ! "$scope" =~ ^[A-Za-z0-9._-]+$ ]]; then
    die_input "SCOPE='${scope}' must be a plain folder name"
fi
fuzz_seconds="${NEUTRALITY_FUZZ_SECONDS:-15}"
[[ "$fuzz_seconds" =~ ^[1-9][0-9]*$ ]] || die_input "NEUTRALITY_FUZZ_SECONDS must be a positive integer"
fuzz_targets=(scanner_vs_vte vt_program_status_stream)

for tool in cargo jq uv git shasum; do
    command -v "$tool" >/dev/null 2>&1 || { echo "✗ missing required command: $tool" >&2; exit 1; }
done

# A dirty tree is built and measured, but the report says so and the VT QA guard refuses it.
dirty=false
# Every input of the measurement, not only the code under test.
inputs=(crates Cargo.toml Cargo.lock rust-toolchain.toml .cargo fuzz .shux/goldens .shux/fixtures
    .shux/scripts/lib .claude/automations/pixel_verify.py scripts/vt-neutrality.sh scripts/fuzz.sh)
# fuzz/Cargo.lock lags each release's version bump, and the fuzz layer rewrites it.
[[ -z "$(git status --porcelain -- "${inputs[@]}" ':(exclude)fuzz/Cargo.lock')" ]] || dirty=true

out_rel=".shux/out/vt-neutrality"
out="$repo_root/$out_rel"
rm -rf "$out"
mkdir -p "$out/metrics"
exec > >(tee "$out/run.log") 2>&1
results="$out/results.tsv"   # layer<TAB>case<TAB>status<TAB>detail
: >"$results"
record() { printf '%s\t%s\t%s\t%s\n' "$1" "$2" "$3" "${4:-}" >>"$results"; }

# shellcheck source=.shux/scripts/lib/shux_harness.sh disable=SC1091  # resolved from the repo root at run time
source "$repo_root/.shux/scripts/lib/shux_harness.sh"
# shellcheck source=.shux/scripts/lib/cache_lock.sh disable=SC1091  # resolved from the repo root at run time
source "$repo_root/.shux/scripts/lib/cache_lock.sh"
runtimes=()
cache_root="${SHUX_NEUTRALITY_CACHE:-${XDG_CACHE_HOME:-$HOME/.cache}/shux/vt-neutrality}"
lock="$cache_root/lock"
holding_lock=false
reader="$cache_root/readers/$$"
cleanup() {
    local rt
    for rt in "${runtimes[@]:-}"; do
        [[ -n "$rt" ]] || continue
        shux_harness_stop_daemon "$rt"
        rm -rf "$rt"
    done
    # Neither failure may stop the other: a stranded lock stalls every later run.
    rm -f "$reader" || echo "⚠ could not remove $reader; make prune ignores it once this pid is gone" >&2
    if [[ "$holding_lock" == true ]]; then
        git worktree remove --force "$cache_root/src" >/dev/null 2>&1 || true  # absent on a cache hit
        cache_lock_release "$lock" || echo "⚠ could not release $lock; the next run replaces it" >&2
    fi
}
trap cleanup EXIT

# ── Builds ──────────────────────────────────────────────────────────────────
# Artifacts are read from target/release, so nothing may move them elsewhere.
unset CARGO_BUILD_TARGET
step "build HEAD (${head:0:10}) release shux + corpus harness"
# The executable path cargo reports, wherever a config puts its artifacts.
built() {
    cargo build --release --locked --message-format=json-render-diagnostics "$@" |
        jq -r 'select(.reason == "compiler-artifact" and .executable != null) | .executable' | tail -1
}
head_bin="$(CARGO_TARGET_DIR="$repo_root/target" built -p shux --bin shux)"
head_harness="$(CARGO_TARGET_DIR="$repo_root/target" built -p shux-raster --example vt_corpus_harness)"
[[ -x "$head_bin" && -x "$head_harness" ]] || { echo "✗ cargo reported no HEAD executables" >&2; exit 1; }

mkdir -p "$cache_root"
probe="$(mktemp -d "${TMPDIR:-/tmp}/shux-neutral-tc.XXXXXX")"
git show "$base:rust-toolchain.toml" >"$probe/rust-toolchain.toml"
toolchain="$(cd "$probe" && rustc -vV && cargo -V)
$(env | grep -E '^(RUSTFLAGS|CARGO_ENCODED_RUSTFLAGS|CARGO_BUILD_|CARGO_PROFILE_|RUSTC_WRAPPER)' | sort || true)
$(cat "${CARGO_HOME:-$HOME/.cargo}/config.toml" 2>/dev/null || true)"
rm -rf "$probe"
toolchain_key="$(printf '%s' "$toolchain" | shasum -a 256 | cut -c1-16)"
bin_dir="$cache_root/bin/${base}-${toolchain_key}"
base_bin="$bin_dir/shux"
base_harness="$bin_dir/vt_corpus_harness"

# make prune leaves the cache alone while a marker in readers/ names a live pid; the
# marker is written under the lock prune holds while it removes anything.
mkdir -p "$cache_root/readers"
if ! cache_lock_take "$lock" 30; then
    echo "✗ $lock held for 30s by a running vt-neutrality or make prune (pid in $lock/pid)" >&2
    exit 1
fi
holding_lock=true
echo "$$" >"$reader"

if [[ -x "$base_bin" && -x "$base_harness" && -f "$bin_dir/stamp" ]]; then
    step "base build cached: ${bin_dir/#$HOME/~}"
    cache_hit=true
    cache_lock_release "$lock"
    holding_lock=false
else
    cache_hit=false
    step "build base (${base:0:10}) into the cache (toolchain ${toolchain_key})"
    git worktree remove --force "$cache_root/src" >/dev/null 2>&1 || true  # leftover from a killed run, if any
    rm -rf "$cache_root/src"
    git worktree add --detach --force "$cache_root/src" "$base" >/dev/null
    built_base_bin="$(cd "$cache_root/src" && CARGO_TARGET_DIR="$cache_root/target" built -p shux --bin shux)"
    built_base_harness="$(cd "$cache_root/src" &&
        CARGO_TARGET_DIR="$cache_root/target" built -p shux-raster --example vt_corpus_harness)"
    rm -rf "$bin_dir.tmp"
    mkdir -p "$bin_dir.tmp"
    cp "$built_base_bin" "$bin_dir.tmp/shux"
    cp "$built_base_harness" "$bin_dir.tmp/vt_corpus_harness"
    printf '%s\n%s\n' "$base" "$toolchain" >"$bin_dir.tmp/stamp"
    rm -rf "$bin_dir"
    mv "$bin_dir.tmp" "$bin_dir"
    git worktree remove --force "$cache_root/src"
    cache_lock_release "$lock"
    holding_lock=false
fi

# ── Comparator ──────────────────────────────────────────────────────────────
# compare <layer> <case> <head.png> <base.png>: exact 0/0, metric kept repo-relative.
compare() {
    local layer="$1" case="$2" actual="$3" expected="$4" metric status
    metric="$out/metrics/${layer}-${case}.json"
    if uv run --quiet --script .claude/automations/pixel_verify.py \
        "${actual#"$repo_root"/}" "${expected#"$repo_root"/}" \
        --diff "$out_rel/metrics/${layer}-${case}-diff.png" \
        --max-pixel-diff-ratio 0 --max-mean-channel-delta 0 >"$metric"; then
        status=pass
    elif jq -e '.reason == "actual_missing" or .reason == "expected_missing"' "$metric" >/dev/null 2>&1; then
        status=error  # nothing was rendered to compare
    else
        status=fail
    fi
    record "$layer" "$case" "$status" "$(jq -c '{changed_pixels, total_pixels, reason}' "$metric" 2>/dev/null || echo '{}')"
    printf '  %s %-16s %-40s %s\n' "$([[ $status == pass ]] && printf '\033[32m✓\033[0m' || printf '\033[31m✗\033[0m')" \
        "$layer" "$case" "$(jq -c '{changed_pixels}' "$metric" 2>/dev/null || true)"
}

# compare_report <layer> <corpus-report.json>: every case's text, replies and
# pixels; prints one line per case.
compare_report() {
    local layer="$1" report="$2" name status actual expected
    while IFS=$'\t' read -r name status actual expected; do
        [[ "$status" == fail ]] && record "$layer" "$name" fail "text or responses differ"
        compare "$layer" "$name" "$actual" "$expected"
    done < <(jq -r '.cases[] | ["\(.layer)-\(.name)", .status, .actual_png, .expected_png] | @tsv' "$report")
}

# ── corpus-goldens ──────────────────────────────────────────────────────────
step "corpus-goldens: HEAD harness vs committed goldens"
goldens_ok=true
"$head_harness" --mode verify --fixtures .shux/fixtures/vt-corpus \
    --goldens .shux/goldens/073-vt-corpus --out "$out/corpus-goldens" || goldens_ok=false
if [[ -f "$out/corpus-goldens/corpus-report.json" ]]; then
    compare_report corpus-goldens "$out/corpus-goldens/corpus-report.json"
    [[ "$goldens_ok" == true ]] || record corpus-goldens harness fail "harness verify exited non-zero"
fi
[[ "$(awk -F'\t' '$1 == "corpus-goldens"' "$results" | wc -l)" -gt 0 ]] ||
    record corpus-goldens harness error "the harness compared no case"

# ── corpus-ab ───────────────────────────────────────────────────────────────
step "corpus-ab: base vs HEAD harness on base's fixtures"
mkdir -p "$out/fixtures"
git archive "$base" .shux/fixtures/vt-corpus | tar -x -C "$out/fixtures"
fixtures="$out/fixtures/.shux/fixtures/vt-corpus"
"$base_harness" --mode promote --fixtures "$fixtures" \
    --goldens "$out/corpus-ab/base" --out "$out/corpus-ab/base-render" >/dev/null
ab_ok=true
"$head_harness" --mode verify --fixtures "$fixtures" \
    --goldens "$out/corpus-ab/base" --out "$out/corpus-ab/head" || ab_ok=false
[[ ! -f "$out/corpus-ab/head/corpus-report.json" ]] ||
    compare_report corpus-ab "$out/corpus-ab/head/corpus-report.json"
ab_cases="$(awk -F'\t' '$1 == "corpus-ab"' "$results" | wc -l | tr -d ' ')"
[[ "$ab_ok" == true ]] || record corpus-ab harness fail "HEAD harness differs from base on text or responses"
[[ "$ab_cases" -gt 0 ]] || record corpus-ab harness error "base fixtures produced no cases"

# ── live-ab + colour-probe ──────────────────────────────────────────────────
# One pane per case. It waits on a line from us until set-size has landed, so no
# byte is ever laid out at the default size. Its end is a file it touches, not a
# drawn marker (a recording may leave autowrap or a scroll region changed) and
# not a shux feature, so any base binary can be measured.
probe_payload() {
    cat <<'EOF2'
printf '\033[38;2;120;220;180mTRUECOLOR\033[0m \033[38;5;208mINDEXED\033[0m \033[34mBASIC\033[0m\n'
printf '\033[1mBOLD\033[0m \033[3mITALIC\033[0m \033[4mUNDER\033[0m \033[7mREVERSE\033[0m \033[9mSTRIKE\033[0m\n'
printf '\342\224\214\342\224\200\342\224\200\342\224\254\342\224\200\342\224\200\342\224\220 box\n'
printf '\346\227\245\346\234\254\350\252\236 wide  e\314\201 combining\n'
printf '\033[48;2;40;40;90m\033[38;2;255;210;90m bg+fg truecolor \033[0m\n'
for i in 1 2 3 4 5 6; do printf 'row %d: \033[3%dmcolour\033[0m 0123456789\n' "$i" "$i"; done
# Every palette entry: the recordings use none of the 256 indexed colours.
i=0; while [ "$i" -lt 256 ]; do printf '\033[48;5;%dm ' "$i"; i=$((i + 1)); [ $((i % 64)) -eq 0 ] && printf '\033[0m\n'; done
for c in 30 31 32 33 34 35 36 37 90 91 92 93 94 95 96 97; do printf '\033[%dmX\033[%dm \033[0m' "$c" "$((c + 10))"; done; printf '\n'
i=0; while [ "$i" -lt 64 ]; do printf '\033[48;2;%d;%d;128m ' "$((i * 4))" "$((255 - i * 4))"; i=$((i + 1)); done; printf '\033[0m\n'
EOF2
}

live_side() {
    local side="$1" bin="$2" case="$3" cols="$4" rows="$5" body="$6"
    local rt="$7" dir="$out/live/$side"
    local -a sx=(env -u SHUX_SOCKET XDG_RUNTIME_DIR="$rt" XDG_CONFIG_HOME="$rt/config" "$bin")
    local script="$rt/$case.sh" done_file="$rt/$case.done" pane i
    mkdir -p "$dir"
    {
        # Echo stays off: the VT's replies to queries in a recording go back
        # down the PTY, and echoing them would draw them at a racy moment.
        printf 'stty -echo; read -r _\n'
        printf '%s\n' "$body"
        printf ': >%q\n' "$done_file"
        printf 'exec sleep 600\n'
    } >"$script"
    # Same session name on both sides: it is drawn in the window chrome.
    "${sx[@]}" session create nt -d --title nt -- \
        env TERM=xterm-256color COLORTERM=truecolor LANG=C.UTF-8 LC_ALL=C.UTF-8 HOME="$rt" \
        sh "$script" >/dev/null
    pane="$("${sx[@]}" --format json pane list -s nt | jq -r '.[0].id')"
    "${sx[@]}" pane set-size -s nt -p "$pane" --cols "$cols" --rows "$rows" >/dev/null
    "${sx[@]}" pane send-keys -s nt -p "$pane" --data Cg== >/dev/null
    for ((i = 0; i < 300; i++)); do
        [[ -e "$done_file" ]] && break
        sleep 0.1
    done
    [[ -e "$done_file" ]] || { echo "✗ live $side $case: the replay never finished" >&2; exit 1; }
    "${sx[@]}" pane wait-settled "$pane" --quiet 250ms --timeout 10s >/dev/null
    "${sx[@]}" pane capture -s nt -p "$pane" --lines "$rows" >"$dir/$case.txt"
    "${sx[@]}" pane snapshot -s nt -p "$pane" -o "$dir/$case-pane.png" >/dev/null
    "${sx[@]}" window snapshot -s nt -o "$dir/$case-window.png" >/dev/null
    "${sx[@]}" session kill nt >/dev/null
}

# Sets RT. Not a $(…): the EXIT trap must see every runtime this shell created.
new_runtime() {
    RT="$(mktemp -d "${TMPDIR:-/tmp}/shuxnt.XXXXXX")"
    mkdir -p "$RT/config/shux"
    printf '[appearance]\nborder_style = "rounded"\n' >"$RT/config/shux/config.toml"
    runtimes+=("$RT")
}

live_cases=()   # name|cols|rows|kind
while IFS=$'\t' read -r name raw cols rows; do
    live_cases+=("$name|$cols|$rows|$raw")
done < <(jq -r '.fixtures[] | [.name, .raw, .cols, .rows] | @tsv' "$fixtures/rich-tui/manifest.json")
for size in 80x24 120x40 200x60; do
    live_cases+=("probe-$size|${size%x*}|${size#*x}|")
done

for side in base head; do
    step "live: ${side} binary replays $(( ${#live_cases[@]} - 3 )) recordings + colour probe at 3 sizes"
    bin="$base_bin"; [[ "$side" == head ]] && bin="$head_bin"
    new_runtime
    rt="$RT"
    for entry in "${live_cases[@]}"; do
        IFS='|' read -r name cols rows raw <<<"$entry"
        if [[ -n "$raw" ]]; then
            body="cat '$fixtures/rich-tui/$raw'"
        else
            body="$(probe_payload)"
        fi
        live_side "$side" "$bin" "$name" "$cols" "$rows" "$body" "$rt"
    done
    env -u SHUX_SOCKET XDG_RUNTIME_DIR="$rt" "$bin" daemon stop >/dev/null
    if shux_harness_assert_no_daemon "$rt"; then
        record leaks "$side" pass
    else
        record leaks "$side" error "daemon still running after daemon stop"
    fi
done

step "live-ab + colour-probe: compare base vs HEAD"
for entry in "${live_cases[@]}"; do
    IFS='|' read -r name _ _ raw <<<"$entry"
    layer=live-ab; [[ -z "$raw" ]] && layer=colour-probe
    if [[ "$layer" == colour-probe ]]; then
        for side in base head; do
            for needle in TRUECOLOR INDEXED BASIC; do
                grep -q -- "$needle" "$out/live/$side/$name.txt" ||
                    record colour-probe "$name-$side" error "capture lacks $needle"
            done
            uv run --quiet --script .shux/scripts/lib/png_not_blank.py \
                "$out/live/$side/$name-pane.png" "$out/live/$side/$name-window.png" \
                --min-colors 8 --min-ink-ratio 0.004 >/dev/null ||
                record colour-probe "$name-$side" error "snapshot is blank or monochrome"
        done
    fi
    if cmp -s "$out/live/base/$name.txt" "$out/live/head/$name.txt"; then
        record "$layer" "$name-text" pass
    else
        record "$layer" "$name-text" fail "pane capture text differs"
    fi
    compare "$layer" "$name-pane" "$out/live/head/$name-pane.png" "$out/live/base/$name-pane.png"
    compare "$layer" "$name-window" "$out/live/head/$name-window.png" "$out/live/base/$name-window.png"
done

# ── fuzz ────────────────────────────────────────────────────────────────────
step "fuzz: ${fuzz_targets[*]} for ${fuzz_seconds}s each"
# One fuzz build dir for every worktree: its instrumented build is the slowest
# step here, and sharing it keeps it incremental across branches.
if CARGO_TARGET_DIR="$cache_root/fuzz-target" bash scripts/fuzz.sh "$fuzz_seconds" "${fuzz_targets[@]}"; then
    record fuzz "${fuzz_targets[*]}" pass "${fuzz_seconds}s each"
else
    record fuzz "${fuzz_targets[*]}" error "crash or build failure; see fuzz/artifacts/"
fi

# ── Report ──────────────────────────────────────────────────────────────────
# pass: every layer 0/0 · moved: every layer ran, some cell or pixel differs ·
# error: a layer could not run, crashed, leaked or compared nothing.
status=pass
awk -F'\t' '$3 == "fail" { bad = 1 } END { exit bad }' "$results" || status=moved
awk -F'\t' '$3 == "error" { bad = 1 } END { exit bad }' "$results" || status=error
metric_count="$(find "$out/metrics" -name '*.json' | wc -l | tr -d ' ')"
jq -Rn \
    --arg base "$base" --arg head "$head" --argjson dirty "$dirty" \
    --arg toolchain_key "$toolchain_key" --argjson cache_hit "$cache_hit" \
    --arg status "$status" --argjson fuzz_seconds "$fuzz_seconds" \
    --argjson metrics "$metric_count" '
    [inputs | split("\t") | {layer: .[0], case: .[1], status: .[2], detail: .[3]}] as $rows
    | {
        schema_version: 1,
        tool: "scripts/vt-neutrality.sh",
        base: $base, head: $head, dirty: $dirty,
        toolchain_key: $toolchain_key, base_cache_hit: $cache_hit,
        fuzz_seconds_per_target: $fuzz_seconds,
        status: $status,
        metrics: $metrics,
        layers: ($rows | group_by(.layer) | map({key: .[0].layer, value: {
            status: (if any(.[]; .status == "error") then "error"
                     elif any(.[]; .status == "fail") then "moved" else "pass" end),
            cases: length,
            failed: [.[] | select(.status != "pass") | {case, status, detail}]
        }}) | from_entries)
    }' <"$results" >"$out/neutrality.json"

if [[ -n "$scope" ]]; then
    qa=".shux/qa/$scope"
    mkdir -p "$qa/neutrality"
    rm -f "$qa"/neutrality/*.json
    cp "$out"/metrics/*.json "$qa/neutrality/"
    jq --arg dir "neutrality" '.metrics_dir = $dir' "$out/neutrality.json" >"$qa/neutrality.json"
    echo "  report + metrics copied to $qa/"
fi

echo "  total ${SECONDS}s"
jq -C '{status, base: .base[0:10], head: .head[0:10], dirty, base_cache_hit,
        layers: (.layers | map_values({status, cases, failed: (.failed | length)}))}' "$out/neutrality.json"
case "$status" in
    pass)
        printf '\033[1;32m✓ vt-neutrality: 0/0 on every layer (base %s → head %s)\033[0m\n' "${base:0:10}" "${head:0:10}" ;;
    moved)
        jq -r '.layers | to_entries[] | .key as $l | .value.failed[] | "  ✗ \($l) \(.case): \(.detail)"' "$out/neutrality.json" >&2
        printf '\033[1;33m✗ vt-neutrality: every layer ran and something moved; diffs in %s/metrics\033[0m\n' "$out_rel" >&2
        exit 3 ;;
    *)
        jq -r '.layers | to_entries[] | .key as $l | .value.failed[] | "  ✗ \($l) \(.case) [\(.status)]: \(.detail)"' "$out/neutrality.json" >&2
        printf '\033[1;31m✗ vt-neutrality: a layer did not run cleanly; nothing here is proof\033[0m\n' >&2
        exit 1 ;;
esac
