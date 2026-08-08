#!/usr/bin/env bash
# Mutation battery for issue #135.
#
# A test that has never been seen failing asserts only that the code does what
# it does. This applies one faithful mutation at a time — each one is the
# pre-fix behaviour, or a plausible wrong version of the fix — and requires a
# NAMED test to catch it.
#
# A mutation whose edit matched nothing is a FAILURE, not a kill: otherwise the
# battery reports a kill it never earned when a refactor moves its anchor.
#
# Two things this battery must prove before it may credit anything, both added
# after review of PR #143 caught it crediting kills it had not earned:
#
#   1. The suites are GREEN unmutated. Every suite runs once before the loop and
#      its failure set must be empty. Without that, one already-red test credits
#      every mutation in its suite and the battery reports 21/21 having proved
#      nothing. The baseline failure set is also subtracted from every later
#      kill, so a test that turns flaky mid-run cannot stand in for an anchor.
#   2. The kill came from the RIGHT test. Each mutation names the anchor that is
#      supposed to catch it, and the anchor must appear in that run's failure
#      set. "Some test failed" is the textbook definition of a killed mutant,
#      but it is not what this file claims: it claims a named test covers each
#      defect, and only an anchor check makes that true.
#
#   .shux/scripts/issue_135_mutation_check.sh
#
# Output: a table of mutation -> killed-by, and a non-zero exit on any survivor.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "${REPO_ROOT}"

STYLE="crates/shux/src/style.rs"
PTY="crates/shux-pty/src/command.rs"
PC="crates/shux/src/pane_command.rs"
BACKUP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/shux-135-mutation-XXXXXX")"
cp "${STYLE}" "${BACKUP_DIR}/style.rs"
cp "${PTY}" "${BACKUP_DIR}/command.rs"
cp "${PC}" "${BACKUP_DIR}/pane_command.rs"

restore() {
  cp "${BACKUP_DIR}/style.rs" "${STYLE}"
  cp "${BACKUP_DIR}/command.rs" "${PTY}"
  cp "${BACKUP_DIR}/pane_command.rs" "${PC}"
}
trap 'restore; rm -rf "${BACKUP_DIR}"' EXIT

# name | anchor test that must catch it | file | python-expression applying the
# mutation to `s`, which must change it. The expression may itself contain `|`
# (Rust match arms do) — it is parsed as "everything after the third field".
MUTATIONS=(
  "argv_joined_with_a_bare_space|style::tests::one_argument_with_a_space_is_distinguishable_from_two|${STYLE}|s.replace('shux_pty::shell_escape_args(argv)', 'argv.join(\" \")')"
  "title_dropped_from_the_plain_arm|style::tests::the_plain_pane_list_carries_the_title_in_a_fourth_column|${STYLE}|s.replace('\"{}\\\\t{}\\\\t{}\\\\t{}\",', '\"{}\\\\t{}\\\\t{}{}\",')"
  "title_column_dropped_from_the_text_arm|style::tests::the_text_pane_list_names_every_pane_and_says_what_it_runs|${STYLE}|s.replace('(PaneField::Title, \"TITLE\", 5),', '')"
  "command_column_dropped_from_the_text_arm|style::tests::the_text_pane_list_names_every_pane_and_says_what_it_runs|${STYLE}|s.replace('(PaneField::Command, \"COMMAND\", 7),', '')"
  "budget_ignored|style::tests::a_budgeted_layout_reports_one_width_to_all_three_callers|${STYLE}|s.replace('let Some(budget) = self.budget else {', 'let Some(budget) = None::<usize> else {')"
  "header_not_trimmed_to_the_box|style::tests::the_box_never_renders_wider_than_the_terminal|${STYLE}|s.replace('fit_width(&header_raw, box_width.saturating_sub(2), ellipsis)', 'header_raw.clone()')"
  "footer_not_trimmed_to_the_box|style::tests::the_box_never_renders_wider_than_the_terminal|${STYLE}|s.replace('fit_width(&footer_raw, box_width.saturating_sub(1), ellipsis)', 'footer_raw.clone()')"
  "header_trimmed_one_column_too_wide|style::tests::the_box_never_renders_wider_than_the_terminal|${STYLE}|s.replace('fit_width(&header_raw, box_width.saturating_sub(2), ellipsis)', 'fit_width(&header_raw, box_width.saturating_sub(1), ellipsis)')"
  "later_floors_not_reserved|style::tests::the_box_never_renders_wider_than_the_terminal|${STYLE}|s.replace('.min(remaining.saturating_sub(later_floors))', '')"
  # The regression the adversarial review found: summing per-character widths
  # instead of measuring the accumulated string. A VS16 emoji is 1 summed and 2
  # as a string, so cells came back up to twice the width they were allocated
  # and the box printed lines wider than the terminal.
  "truncation_sums_per_character_widths|style::tests::a_fitted_cell_is_exactly_as_wide_as_it_was_asked_to_be|${STYLE}|s.replace('        if display_width(&candidate) > width {', '        if candidate.chars().map(|c| UnicodeWidthStr::width(c.to_string().as_str())).sum::<usize>() > width {')"
  "truncation_counts_chars_not_columns|style::tests::a_fitted_cell_is_exactly_as_wide_as_it_was_asked_to_be|${STYLE}|s.replace('        if display_width(&candidate) > width {', '        if candidate.chars().count() > width {')"
  "truncation_may_end_on_a_zero_width_char|style::tests::truncation_never_ends_on_a_zero_width_character|${STYLE}|s.replace('''    while out\n        .chars()\n        .next_back()\n        .is_some_and(|c| display_width(&out) == display_width(&out[..out.len() - c.len_utf8()]))\n    {\n        out.pop();\n    }\n''', '')"
  # The minimum-width constant must stay DERIVED from the widest marker. Written
  # down as 24 it silently excluded every zoomed pane.
  "min_boxable_width_forgets_the_zoomed_marker|style::tests::the_box_never_renders_wider_than_the_terminal|${STYLE}|s.replace('    2 + 8 + GAP + display_width(&pane_marker(true, true)) + 2', '    24')"
  # NOT a mutation: swapping the guard and the quoting is provably equivalent
  # (every character `safe_label` rewrites already fails the quoting allowlist;
  # 200,000 random argvs, zero differences), so it is an equivalent mutant and
  # counting it as a survivor would be counting a kill nobody can earn. What IS
  # observable is dropping the guard, on either field:
  "egress_guard_dropped_from_the_command|style::tests::a_control_character_cannot_forge_a_plain_column|${STYLE}|s.replace('safe_label(&render_argv(&p.command)),', 'render_argv(&p.command),')"
  "egress_guard_dropped_from_the_title|style::tests::a_control_character_cannot_forge_a_plain_column|${STYLE}|s.replace('                    safe_label(&p.title),\n                );', '                    p.title.clone(),\n                );')"
  "quoting_back_to_the_denylist|command::tests::a_metacharacter_argument_is_one_word_not_a_second_command|${PTY}|s.replace('''    let unquoted_is_literal = arg.bytes().all(|b| {\n        b.is_ascii_alphanumeric()\n            || matches!(\n                b,\n                b'_' | b'-' | b'.' | b'/' | b',' | b':' | b'=' | b'+' | b'@' | b'%'\n            )\n    });''', '''    let unquoted_is_literal = !(arg.contains(' ') || arg.contains('\\\\'') || arg.contains('\$'));''')"
  "empty_argument_not_quoted|command::tests::an_empty_argument_survives_instead_of_vanishing|${PTY}|s.replace('''    if arg.is_empty() {\n        return \"''\".to_string();\n    }''', '')"
  "leading_equals_not_quoted|command::tests::the_quoting_holds_in_every_installed_shell|${PTY}|s.replace(\"    if arg.starts_with('=') {\", '    if false {')"
  # A pane whose zoom is invisible in both human formats while `--format json`
  # reports it.
  "zoom_hidden_without_focus|style::tests::a_zoomed_pane_is_marked_even_when_it_does_not_have_focus|${STYLE}|s.replace('(false, true) => \"[zoomed]\".to_string(),', '(false, true) => String::new(),')"
  # `args` is TYPED INTO A TERMINAL. Correct quoting put the control byte inside
  # the quotes, so the line discipline's truncation left an unterminated quote
  # and wedged the pane for good.
  "control_bytes_allowed_into_a_typed_line|pane_command::tests::a_control_character_in_run_args_is_rejected|${PC}|s.replace(\"(*c as u32) < 0x20 || *c == '\\\\u{7f}'\", 'false')"
  "only_the_three_signal_bytes_rejected|pane_command::tests::a_control_character_in_run_args_is_rejected|${PC}|s.replace(\"(*c as u32) < 0x20 || *c == '\\\\u{7f}'\", \"matches!(*c, '\\\\u{3}' | '\\\\u{15}' | '\\\\u{1a}')\")"
)

# Run the suite that owns `file` and echo its log, through a make target rather
# than raw cargo (CLAUDE.md). `--color never` is pinned inside that target: CI
# exports CARGO_TERM_COLOR=always, and the parser below anchors on plain text. A non-zero exit is EXPECTED here (failing tests are the signal),
# so it is swallowed deliberately — but `has_run` below then proves cargo
# actually got as far as running tests, so a build or harness failure can never
# masquerade as a result.
run_suite() {
  case "$1" in
    "${PTY}") make -s test-mutation-suite CRATE=shux-pty TARGET=--lib FILTER= 2>&1 || true ;;
    "${PC}") make -s test-mutation-suite CRATE=shux TARGET="--bin shux" FILTER=pane_command::tests 2>&1 || true ;;
    *) make -s test-mutation-suite CRATE=shux TARGET="--bin shux" FILTER=style::tests 2>&1 || true ;;
  esac
}

# Names in libtest's `failures:` block are indented four spaces. Digits belong in
# the class: `test_strip_ansi_8bit_csi` exists, and a parser that cannot see it
# would drop a real killer on the floor.
failures_of() {
  printf '%s\n' "$1" | sed -n 's/^    \([a-z0-9_:]*\)$/\1/p' | sort -u | tr '\n' ' '
}

has_run() { printf '%s\n' "$1" | grep -q '^test result:'; }

# ---------------------------------------------------------------------------
# Baseline. Credit nothing until the unmutated suites are proven green.
# ---------------------------------------------------------------------------
declare -A BASELINE
echo "Baseline (unmutated):"
for f in "${PTY}" "${PC}" "${STYLE}"; do
  log="$(run_suite "${f}")"
  if ! has_run "${log}"; then
    echo "  ${f}: cargo never ran the tests — build or harness failure, not a result." >&2
    printf '%s\n' "${log}" | tail -30 >&2
    exit 2
  fi
  fails="$(failures_of "${log}")"
  if [ -n "${fails}" ]; then
    echo "  ${f}: ALREADY RED — ${fails}" >&2
    echo "Refusing to run: a red baseline credits every mutation in this suite." >&2
    exit 2
  fi
  BASELINE["${f}"]=""
  printf '  %-42s green\n' "${f}"
done
echo

printf '%-46s %s\n' "MUTATION" "KILLED BY"
printf '%-46s %s\n' "----------------------------------------------" "---------"

survivors=()
for entry in "${MUTATIONS[@]}"; do
  name="${entry%%|*}"
  rest="${entry#*|}"
  anchor="${rest%%|*}"
  rest="${rest#*|}"
  file="${rest%%|*}"
  expr="${rest#*|}"

  restore
  if ! MUT_FILE="${file}" MUT_EXPR="${expr}" python3 - <<'PY'
import os, sys
path = os.environ["MUT_FILE"]
expr = os.environ["MUT_EXPR"]
s = open(path).read()
out = eval(expr, {"s": s})
if out == s:
    sys.stderr.write("mutation matched nothing\n")
    sys.exit(3)
open(path, "w").write(out)
PY
  then
    printf '%-46s %s\n' "${name}" "PATCH MATCHED NOTHING"
    survivors+=("${name} (patch matched nothing)")
    continue
  fi

  log="$(run_suite "${file}")"

  if ! has_run "${log}"; then
    # Compile failure counts as a survivor: a mutation must be a behaviour
    # change the tests catch, not a syntax error.
    if printf '%s\n' "${log}" | grep -q '^error'; then
      printf '%-46s %s\n' "${name}" "DID NOT COMPILE"
      survivors+=("${name} (did not compile)")
      continue
    fi
    echo "${name}: cargo never ran the tests and did not report a compile error." >&2
    printf '%s\n' "${log}" | tail -30 >&2
    exit 2
  fi

  # Subtract the baseline so a test that turns flaky mid-run cannot be mistaken
  # for this mutation's killer. The baseline gate proves it starts empty.
  killers=""
  for t in $(failures_of "${log}"); do
    case " ${BASELINE["${file}"]} " in *" ${t} "*) continue ;; esac
    killers="${killers}${t} "
  done
  killers="${killers% }"

  if [ -z "${killers}" ]; then
    printf '%-46s %s\n' "${name}" "SURVIVED"
    survivors+=("${name}")
  elif ! printf '%s\n' " ${killers} " | grep -qF " ${anchor} "; then
    # Something failed, but not the test this defect is documented as covered
    # by. Crediting that would let an unrelated test stand in for the anchor —
    # the battery would stay green through a refactor that moved the real
    # coverage away.
    printf '%-46s %s\n' "${name}" "ANCHOR MISSED (want ${anchor}, got ${killers})"
    survivors+=("${name} (anchor ${anchor} did not fire; failures: ${killers})")
  else
    printf '%-46s %s\n' "${name}" "${killers}"
  fi
done

restore
echo
if [ "${#survivors[@]}" -ne 0 ]; then
  echo "SURVIVORS (${#survivors[@]}):"
  printf '  - %s\n' "${survivors[@]}"
  exit 1
fi
echo "All ${#MUTATIONS[@]} mutations killed."
