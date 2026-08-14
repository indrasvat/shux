#!/usr/bin/env bash
# Assert `crates/shux/src/main.rs` declares no modules.
#
# `crates/shux` has both a `[lib]` and a `[[bin]]`. A module declared in BOTH
# `lib.rs` and `main.rs` is compiled once per target, producing two unrelated
# types with the same name and the same path. Nothing fails to build. What fails
# is everything afterwards: a `Cli` the binary parsed cannot be passed to a
# `run_client` the library exported, and the error says the two `Cli` types are
# different without being able to say how.
#
# It is also the cheapest possible thing to check — `main.rs` owns `fn main()`
# and the module tree lives in `lib.rs`, so the correct number of `mod`
# declarations in this file is zero, for ever. Hence a guard rather than a note.
#
# Only `main.rs` is inspected. `lib.rs` is where modules belong, and every other
# binary-less crate in the workspace is unaffected by this hazard.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${REPO_ROOT}"

MAIN="crates/shux/src/main.rs"

if [[ ! -f "${MAIN}" ]]; then
  echo "error: ${MAIN} does not exist — this guard is checking nothing" >&2
  exit 2
fi

# Declarations only. `mod foo;` and `pub mod foo;` at any indentation, but not
# `mod tests { … }` (an inline block declares nothing that could collide across
# targets) and not the word `mod` inside a comment or string.
#
# `grep -c` exits 1 on no match, which is the passing case here, so the count is
# captured with a `|| true` on the SUBSTITUTION and the emptiness handled below
# — never `grep … || true` as a condition, which would always be true.
hits="$(grep -nE '^[[:space:]]*(pub[[:space:]]+)?mod[[:space:]]+[a-z_][a-z0-9_]*[[:space:]]*;' "${MAIN}" || true)"

if [[ -n "${hits}" ]]; then
  echo "✗ ${MAIN} declares modules:" >&2
  printf '%s\n' "${hits}" | sed 's/^/    /' >&2
  echo "" >&2
  echo "  The module tree belongs to crates/shux/src/lib.rs. A module declared in" >&2
  echo "  both targets compiles into each as a separate type with the same name;" >&2
  echo "  it builds, and every type error after it is unreadable." >&2
  echo "" >&2
  echo "  Move the declaration to lib.rs and reach it from main.rs as \`shux::<name>\`." >&2
  exit 1
fi

echo "✓ ${MAIN} declares no modules"
