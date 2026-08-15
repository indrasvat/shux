#!/usr/bin/env bash
# Assert the `shux` BINARY target declares no modules.
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
# declarations in that file is zero, for ever. Hence a guard rather than a note.
#
# ── What this is, and is not ────────────────────────────────────────────────
# This is a LINT over one small file, not a Rust parser. It is deliberately
# biased toward false POSITIVES: the file it inspects is expected to contain
# zero module declarations, so a spurious hit costs one line of author time,
# while a miss costs the confusing-type-error class this exists to prevent.
#
# The first version of this guard was regex-per-line and an adversarial review
# walked straight through it: `pub(crate) mod foo;` evaded (the regex wanted
# `pub` + whitespace), as did `pub(super)`, `pub(in crate::x)`, an attribute on
# the same line (`#[cfg(test)] mod tests;`), a raw identifier (`mod r#gen;`),
# an uppercase name, and a declaration wrapped across two lines. All of those
# are caught now, which is why the scan below normalises before it matches
# instead of trusting one line to hold one declaration.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${REPO_ROOT}"

MANIFEST="crates/shux/Cargo.toml"
[[ -f "${MANIFEST}" ]] || { echo "error: ${MANIFEST} does not exist" >&2; exit 2; }

# Read the bin path from the manifest rather than hardcoding it. A guard that
# names `src/main.rs` while `[[bin]] path` points somewhere else is checking a
# file the binary does not build — it passes while the hazard sits in the real
# entry point. Adversarial review reproduced exactly that.
MAIN="$(awk '
  /^\[\[bin\]\]/ { inbin = 1; next }
  /^\[/          { inbin = 0 }
  inbin && /^[[:space:]]*path[[:space:]]*=/ {
    line = $0
    sub(/^[^=]*=[[:space:]]*/, "", line)
    gsub(/^"|"[[:space:]]*$/, "", line)
    print "crates/shux/" line
    exit
  }
' "${MANIFEST}")"

if [[ -z "${MAIN}" ]]; then
  echo "error: could not read [[bin]] path from ${MANIFEST} — this guard would check nothing" >&2
  exit 2
fi

if [[ ! -f "${MAIN}" ]]; then
  echo "error: ${MAIN} (from ${MANIFEST}) does not exist — this guard is checking nothing" >&2
  exit 2
fi

# Strip what a `mod` keyword can legally hide inside, then look for declarations
# across the whole file rather than line by line:
#
#   - block comments, line comments, and string/char literals are blanked, so
#     prose and raw strings cannot trip the match (`//! see `mod foo;`` is fine);
#   - `mod NAME ;` is matched with the declaration's pieces allowed to be
#     separated by any whitespace INCLUDING newlines;
#   - visibility (`pub`, `pub(crate)`, `pub(super)`, `pub(in path)`) and any
#     leading attributes are permitted before the keyword, because every one of
#     them is still a declaration;
#   - `mod NAME { … }` (an inline module) is NOT a hit: it declares no separate
#     compilation unit and cannot collide across targets.
#
# The line number reported is the line the `mod` keyword sits on.
hits="$(awk '
  { lines[NR] = $0 }
  END {
    # A literal single quote cannot appear inside this single-quoted awk program,
    # so it is built from its character code and compared by variable throughout.
    SQ = sprintf("%c", 39)
    # Rebuild the file with comments and literals blanked, tracking line numbers.
    inblock = 0
    for (i = 1; i <= NR; i++) {
      s = lines[i]; out = ""
      j = 1
      while (j <= length(s)) {
        c = substr(s, j, 1); d = substr(s, j, 2)
        if (inblock) {
          if (d == "*/") { inblock = 0; j += 2 } else { j++ }
          out = out " "
          continue
        }
        if (d == "/*") { inblock = 1; j += 2; out = out "  "; continue }
        if (d == "//") { while (j <= length(s)) { out = out " "; j++ } break }
        # CHARACTER LITERALS FIRST. A char literal holding a double quote —
        # `const Q: char = SQ "SQ ; mod attach;` — otherwise reads as the START of
        # a string, blanks the rest of the line, and hides the declaration. That
        # is the exact duplicate-module hazard this guard exists to catch, and a
        # bot review found it here. A single quote also introduces a LIFETIME
        # (`SQ a`), which has no closing quote, so a literal is consumed only when
        # a closing quote is actually found; otherwise it is ordinary code.
        if (c == SQ) {
          k2 = 0
          if (substr(s, j + 1, 1) == "\\") {
            # Escaped: newline, backslash, quote, or a \u{...} escape.
            for (m = j + 2; m <= length(s) && m <= j + 12; m++) {
              if (substr(s, m, 1) == SQ) { k2 = m; break }
            }
          } else if (substr(s, j + 2, 1) == SQ && substr(s, j + 1, 1) != SQ) {
            k2 = j + 2
          }
          if (k2 > 0) {
            for (m = j; m <= k2; m++) { out = out " " }
            j = k2 + 1
            continue
          }
          # A lifetime, or a stray quote. Emit it and carry on.
          out = out c; j++
          continue
        }
        if (c == "\"") {
          out = out " "; j++
          while (j <= length(s)) {
            if (substr(s, j, 1) == "\\") { out = out "  "; j += 2; continue }
            if (substr(s, j, 1) == "\"") { out = out " "; j++; break }
            out = out " "; j++
          }
          continue
        }
        out = out c; j++
      }
      clean[i] = out
      joined = joined out "\n"
      # Record, for each character offset in `joined`, which line it came from.
      for (k = 0; k <= length(out); k++) { lineof[offset + k] = i }
      offset += length(out) + 1
    }

    # Collapse newlines to spaces for matching, keeping offsets aligned.
    flat = joined
    gsub(/\n/, " ", flat)

    # `mod NAME ;` with arbitrary whitespace between the pieces. `r#` prefixes
    # and any identifier case are allowed. A trailing `{` is an inline module
    # and is deliberately not matched.
    rest = flat; base = 0
    while (match(rest, /(^|[^A-Za-z0-9_])mod[[:space:]]+(r#)?[A-Za-z_][A-Za-z0-9_]*[[:space:]]*;/)) {
      start = base + RSTART
      # Skip the leading non-identifier character the regex had to consume.
      probe = substr(rest, RSTART, RLENGTH)
      if (probe !~ /^mod/) start += 1
      decl = substr(rest, RSTART, RLENGTH)
      sub(/^[^A-Za-z0-9_]/, "", decl)
      gsub(/[[:space:]]+/, " ", decl)
      printf "%d:%s\n", lineof[start - 1], decl
      base += RSTART + RLENGTH - 1
      rest = substr(rest, RSTART + RLENGTH)
    }
  }
' "${MAIN}")"

if [[ -n "${hits}" ]]; then
  echo "✗ ${MAIN} declares modules:" >&2
  printf '%s\n' "${hits}" | sed 's/^/    /' >&2
  echo "" >&2
  echo "  The module tree belongs to crates/shux/src/lib.rs. A module declared in" >&2
  echo "  both targets compiles into each as a separate type with the same name;" >&2
  echo "  it builds, and every type error after it is unreadable." >&2
  echo "" >&2
  echo "  Move the declaration to lib.rs and reach it from the binary as" >&2
  echo "  \`shux::<name>\`." >&2
  exit 1
fi

echo "✓ ${MAIN} declares no modules"
