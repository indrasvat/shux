#!/usr/bin/env bash
# Mutation check for issue #122 (REP source).
#
# A test that has only ever been seen passing is not evidence. This applies each
# mutation to the fix in turn, re-runs the REP suites, and requires that the run
# FAILS -- naming which test killed it. A mutation that survives is a hole in the
# suite, and the script exits non-zero for it. So does a mutation whose edit
# matched nothing: a vacuous mutation would report a kill it never earned.
#
# `crates/shux-vt/src/parser.rs` must match the index; every mutation is reverted
# with `git checkout`.

set -uo pipefail

repo_root="$(git rev-parse --show-toplevel)"
# `|| exit` is load-bearing here, not boilerplate: this script has `set -uo
# pipefail` but deliberately NOT `-e`, and it rewrites a TRACKED source file in
# place. A failed `cd` would leave it mutating whatever directory it happened to
# start in. Never mask a failure in a measurement harness.
cd "${repo_root}" || exit 1

PARSER=crates/shux-vt/src/parser.rs

# EXCLUSIVE LOCK. This script rewrites a tracked source file in place, over and
# over, for several minutes. Anything else compiling or testing the workspace at
# the same time sees a mutant and reports a failure that does not exist -- the
# VT gate hit exactly that during the issue #122 audit, measuring `parser.rs`
# dirty in 49 of 60 one-second samples and nearly filing a phantom P0. Take the
# lock or refuse to run; never mutate a shared tree opportunistically.
LOCK=.git/shux-mutation-check.lock
exec 9>"${LOCK}"
if ! flock -n 9; then
  echo "FATAL: another mutation run holds ${LOCK}; refusing to rewrite ${PARSER} underneath it" >&2
  exit 1
fi

if ! git diff --quiet -- "${PARSER}"; then
  echo "FATAL: ${PARSER} differs from the index; refusing to mutate" >&2
  exit 1
fi

survivors=0
killed=0
current_name=""

revert() { git checkout -- "${PARSER}"; }
trap revert EXIT

# mutate <name>  -- the python edit is read from stdin and operates on `s`.
mutate() {
  current_name="$1"
  revert
  local before after edit
  before="$(md5sum "${PARSER}")"
  edit="$(cat)"
  MUT_EDIT="${edit}" python3 -c '
import os, sys
p = sys.argv[1]
s = open(p).read()
exec(os.environ["MUT_EDIT"])
open(p, "w").write(s)
' "${PARSER}"
  after="$(md5sum "${PARSER}")"
  if [ "${before}" = "${after}" ]; then
    printf '  VACUOUS  %-52s (the edit matched nothing)\n' "${current_name}"
    survivors=$((survivors + 1))
    return 1
  fi

  local out status
  # `--color never`: the grep below anchors on `^    <test name>`, and cargo
  # colours that line whenever CARGO_TERM_COLOR=always is exported — which CI
  # does workflow-wide, and which a developer may well have in their profile.
  # An ANSI prefix makes the anchor miss and every killed mutant gets reported
  # as "<did not compile>". A parser pins its own input format; it cannot rely
  # on the environment to leave the bytes alone.
  out="$(cargo test --color never -p shux-vt --test rep --lib 2>&1)"
  status=$?
  if [ "${status}" -eq 0 ]; then
    printf '  SURVIVED %-52s <-- no test caught this\n' "${current_name}"
    survivors=$((survivors + 1))
    return 1
  fi
  local first
  first="$(printf '%s\n' "${out}" | grep -oE '^    [a-z_][a-z_0-9]*(::[a-z_0-9]+)*$' | head -1 | tr -d ' ')"
  if [ -z "${first}" ]; then first="<did not compile>"; fi
  printf '  killed   %-52s by %s\n' "${current_name}" "${first}"
  killed=$((killed + 1))
  return 0
}

echo "==> mutation check, issue #122 (holding ${LOCK}; ${PARSER} is rewritten in place)"

mutate "source the screen again (the original bug)" <<'EDIT'
s = s.replace(
    """        let Some(source) = self.last_graphic.clone() else {
            return;
        };""",
    """        let source = {
            let col = if self.cursor.auto_wrap_pending {
                self.cursor.col
            } else if let Some(col) = self.cursor.col.checked_sub(1) {
                col
            } else {
                return;
            };
            match self.grid.visible_row(self.cursor.row).get(col).cloned() {
                Some(cell) => LastGraphic { ch: cell.ch, rest: String::new() },
                None => return,
            }
        };""")
EDIT

mutate "RIS no longer clears the remembered character" <<'EDIT'
s = s.replace("                *self.last_graphic = None;\n", "")
EDIT

mutate "combining marks stop extending the cluster" <<'EDIT'
old = """        if appended {
            self.extend_remembered_graphic((row, col), ch);
        }
"""
assert old in s, "anchor moved"
s = s.replace(old, "")
EDIT

mutate "flag pairs form across a cursor move again" <<'EDIT'
# Anchored on the RI function's own body -- the bare `active_grapheme_position()`
# line appears three times, and replacing the first one mutates a different join.
old = """        // way; this one was not.
        let Some((row, col)) = self.active_grapheme_position() else {"""
assert s.count(old) == 1, "anchor moved or is no longer unique"
s = s.replace(old, """        // way; this one was not.
        let Some((row, col)) = self.preceding_cell_position() else {""")
EDIT

mutate "a stray mark redefines the preceding character" <<'EDIT'
old = """        if *self.active_grapheme_cell != Some(joined) {
            return;
        }
"""
assert old in s, "anchor moved"
s = s.replace(old, "        let _ = joined;\n")
EDIT

mutate "a joining scalar restarts the record instead of extending it" <<'EDIT'
old = """        if self.try_append_to_active_grapheme(ch, width) {
            return;
        }
        if self.try_append_regional_indicator_pair(ch) {
            return;
        }
        self.remember_graphic_scalar(ch);"""
assert old in s, "anchor moved"
s = s.replace(old, """        self.remember_graphic_scalar(ch);
        if self.try_append_to_active_grapheme(ch, width) {
            return;
        }
        if self.try_append_regional_indicator_pair(ch) {
            return;
        }""")
EDIT

mutate "ZWJ joins stop extending the cluster" <<'EDIT'
old = """        self.extend_remembered_graphic((row, col), ch);
        let next_col = col + target_width;"""
assert old in s, "anchor moved"
s = s.replace(old, "        let next_col = col + target_width;")
EDIT

mutate "flag pairs stop extending the cluster" <<'EDIT'
old = """        self.extend_remembered_graphic((row, col), ch);
        self.set_active_grapheme_cell(row, col);
        self.cursor.col = (col + target_width)"""
assert old in s, "anchor moved"
s = s.replace(old, """        self.set_active_grapheme_cell(row, col);
        self.cursor.col = (col + target_width)""")
EDIT

mutate "any control sequence forgets the character" <<'EDIT'
s = s.replace("""        if !continues_the_cluster {
            self.clear_active_grapheme_cell();
        }""", """        if !continues_the_cluster {
            self.clear_active_grapheme_cell();
            *self.last_graphic = None;
        }""")
EDIT

mutate "REP breaks the cluster under construction" <<'EDIT'
old = "intermediates.is_empty() && (action == 'm' || action == 'b')"
assert old in s, "anchor moved"
s = s.replace(old, "intermediates.is_empty() && action == 'm'")
EDIT

mutate "iteration clamp removed" <<'EDIT'
s = s.replace("        count.min(cells).min(scalar_budget.max(1))",
              "        let _ = (cells, scalar_budget);\n        count")
EDIT

mutate "scalar budget removed" <<'EDIT'
s = s.replace("        count.min(cells).min(scalar_budget.max(1))",
              "        let _ = scalar_budget;\n        count.min(cells)")
EDIT

mutate "the character is remembered before translation" <<'EDIT'
s = s.replace("        self.write_char(self.translate_printable(ch));",
              "        let translated = self.translate_printable(ch);\n"
              "        self.write_char(translated);\n"
              "        self.remember_graphic_scalar(ch);")
EDIT

mutate "an explicit count of 0 repeats nothing" <<'EDIT'
s = s.replace("""        if count == 0 {
            return;
        }""", """        if count <= 1 {
            return;
        }""")
EDIT

mutate "the repeats take the original pen, not the current one" <<'EDIT'
s = s.replace("""            for ch in source.scalars() {
                self.write_char(ch);
            }""", """            let saved = self.cursor.style;
            self.cursor.style = Default::default();
            for ch in source.scalars() {
                self.write_char(ch);
            }
            self.cursor.style = saved;""")
EDIT

mutate "a repeat is written once regardless of the count" <<'EDIT'
s = s.replace("""        for _ in 0..self.repeat_iterations(count, &source) {""",
              """        for _ in 0..self.repeat_iterations(count, &source).min(1) {""")
EDIT

mutate "scalar budget loses its .max(1) floor" <<'EDIT'
old = "        count.min(cells).min(scalar_budget.max(1))"
assert old in s, "anchor moved"
s = s.replace(old, "        count.min(cells).min(scalar_budget)")
EDIT

mutate "a wide char dropped for want of room is not remembered" <<'EDIT'
# The faithful mutant is NOT `last_graphic = None` -- that makes REP a no-op,
# which looks identical to the correct behaviour. It is failing to record the
# dropped character at all, so the OLDER one survives and REP draws that.
old = "        self.remember_graphic_scalar(ch);"
assert s.count(old) == 1, "anchor moved or is no longer unique"
s = s.replace(old, "        if !(width == 2 && self.grid.cols() < 2) {\n            self.remember_graphic_scalar(ch);\n        }")
EDIT

revert
echo
echo "==> ${killed} killed, ${survivors} survived"
[ "${survivors}" -eq 0 ]
