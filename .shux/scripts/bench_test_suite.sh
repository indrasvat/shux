#!/usr/bin/env bash
# Benchmark the workspace test suite: the serial legacy runner vs the parallel
# nextest runner, on whatever machine you happen to be sitting at.
#
# Both arms are measured with hyperfine so the numbers carry a mean, a standard
# deviation and a min/max — a single stopwatch reading on a shared box is noise,
# not a measurement.
#
#   .shux/scripts/bench_test_suite.sh              # both arms, 3 runs each
#   RUNS=5 .shux/scripts/bench_test_suite.sh       # more samples
#   ARMS=after .shux/scripts/bench_test_suite.sh   # only the parallel arm
#
# Results land in .shux/out/issue-130/ (gitignored scratch) as hyperfine JSON +
# a markdown summary.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "${REPO_ROOT}"

OUT_DIR="${OUT_DIR:-${REPO_ROOT}/.shux/out/issue-130}"
RUNS="${RUNS:-3}"
WARMUP="${WARMUP:-1}"
ARMS="${ARMS:-before,after}"
LABEL="${LABEL:-$(uname -s)-$(uname -m)}"

mkdir -p "${OUT_DIR}"

if ! command -v hyperfine >/dev/null 2>&1; then
  echo "error: hyperfine is not installed. Run: make setup-bench" >&2
  exit 2
fi

# A short, isolated runtime dir. Unix socket paths are capped near 104 bytes, so
# a long TMPDIR silently truncates the daemon socket path and the suite fails in
# a way that looks like a hang.
runtime_dir="$(mktemp -d "/tmp/shux-bench.XXXXXX")"
export XDG_RUNTIME_DIR="${runtime_dir}"
cleanup() { rm -rf "${runtime_dir}"; }
trap cleanup EXIT

# Build once, outside the measured window. We are benchmarking test EXECUTION;
# folding a compile into the first sample would measure the compiler.
echo "▶ Warming the build (not measured)..."
cargo test --workspace --no-run >/dev/null 2>&1
if command -v cargo-nextest >/dev/null 2>&1; then
  cargo nextest run --workspace --no-run >/dev/null 2>&1
fi

nproc_count="$( (command -v nproc >/dev/null && nproc) || sysctl -n hw.ncpu || echo '?')"
echo "▶ Host: ${LABEL}, ${nproc_count} cores"

declare -a names=() cmds=()

if [[ ",${ARMS}," == *",before,"* ]]; then
  names+=("before: serial (run-cargo-test.sh --test-threads=1)")
  cmds+=(".shux/scripts/no_leak_guard.sh bash scripts/run-cargo-test.sh --workspace -- --test-threads=1")
fi

if [[ ",${ARMS}," == *",after,"* ]]; then
  if ! command -v cargo-nextest >/dev/null 2>&1; then
    echo "error: cargo-nextest is not installed; the 'after' arm cannot run." >&2
    echo "       Run: make setup-nextest" >&2
    exit 2
  fi
  names+=("after: parallel (cargo nextest run --workspace)")
  cmds+=(".shux/scripts/no_leak_guard.sh cargo nextest run --workspace --no-fail-fast")
fi

if [[ "${#cmds[@]}" -eq 0 ]]; then
  echo "error: ARMS selected no arms (got '${ARMS}')" >&2
  exit 2
fi

json_out="${OUT_DIR}/suite-${LABEL}.json"
md_out="${OUT_DIR}/suite-${LABEL}.md"

hyperfine_args=(--warmup "${WARMUP}" --runs "${RUNS}" --export-json "${json_out}" --export-markdown "${md_out}")
for i in "${!cmds[@]}"; do
  hyperfine_args+=(--command-name "${names[$i]}" "${cmds[$i]}")
done

echo "▶ hyperfine: ${RUNS} runs per arm, ${WARMUP} warmup"
hyperfine "${hyperfine_args[@]}"

echo
echo "✓ JSON:     ${json_out}"
echo "✓ Markdown: ${md_out}"
