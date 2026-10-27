#!/usr/bin/env bash
# Run libFuzzer targets in fuzz/ for a fixed time each, seeded from
# fuzz/seeds/<target>. Any crash fails the run; its input is left under
# fuzz/artifacts/<target>/ for a tier-1 regression test.
#
#   scripts/fuzz.sh SECONDS TARGET...
set -euo pipefail

seconds="${1:?usage: fuzz.sh SECONDS TARGET...}"
shift
[ "$#" -gt 0 ] || { echo "fuzz.sh: no targets given" >&2; exit 2; }

if ! cargo fuzz --version >/dev/null 2>&1; then
    echo "fuzz.sh: cargo-fuzz is not installed (cargo install cargo-fuzz)" >&2
    exit 1
fi

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root/fuzz"
for target in "$@"; do
    [ -f "fuzz_targets/$target.rs" ] || { echo "fuzz.sh: unknown target $target" >&2; exit 2; }
    mkdir -p "corpus/$target"
    if [ -d "seeds/$target" ]; then
        cp -n seeds/"$target"/* "corpus/$target/" 2>/dev/null || true  # seeds are optional; a re-run keeps grown corpus
    fi
    echo "▶ fuzz $target for ${seconds}s"
    # -s none: libFuzzer coverage without sanitizers, so the pinned stable
    # toolchain builds it.
    cargo fuzz run -s none "$target" "corpus/$target" -- \
        -max_total_time="$seconds" -dict=dict/osc7501.dict -print_final_stats=1
    echo "✓ fuzz $target"
done
