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

# name | file | python-expression applying the mutation to `s`, must change it
MUTATIONS=(
  "argv_joined_with_a_bare_space|${STYLE}|s.replace('shux_pty::shell_escape_args(argv)', 'argv.join(\" \")')"
  "title_dropped_from_the_plain_arm|${STYLE}|s.replace('\"{}\\\\t{}\\\\t{}\\\\t{}\",', '\"{}\\\\t{}\\\\t{}{}\",')"
  "title_column_dropped_from_the_text_arm|${STYLE}|s.replace('(PaneField::Title, \"TITLE\", 5),', '')"
  "command_column_dropped_from_the_text_arm|${STYLE}|s.replace('(PaneField::Command, \"COMMAND\", 7),', '')"
  "budget_ignored|${STYLE}|s.replace('let Some(budget) = self.budget else {', 'let Some(budget) = None::<usize> else {')"
  "header_not_trimmed_to_the_box|${STYLE}|s.replace('fit_width(&header_raw, box_width.saturating_sub(2), ellipsis)', 'header_raw.clone()')"
  "footer_not_trimmed_to_the_box|${STYLE}|s.replace('fit_width(&footer_raw, box_width.saturating_sub(1), ellipsis)', 'footer_raw.clone()')"
  "header_trimmed_one_column_too_wide|${STYLE}|s.replace('fit_width(&header_raw, box_width.saturating_sub(2), ellipsis)', 'fit_width(&header_raw, box_width.saturating_sub(1), ellipsis)')"
  "later_floors_not_reserved|${STYLE}|s.replace('.min(remaining.saturating_sub(later_floors))', '')"
  # The regression the adversarial review found: summing per-character widths
  # instead of measuring the accumulated string. A VS16 emoji is 1 summed and 2
  # as a string, so cells came back up to twice the width they were allocated
  # and the box printed lines wider than the terminal.
  "truncation_sums_per_character_widths|${STYLE}|s.replace('        if display_width(&candidate) > width {', '        if candidate.chars().map(|c| UnicodeWidthStr::width(c.to_string().as_str())).sum::<usize>() > width {')"
  "truncation_counts_chars_not_columns|${STYLE}|s.replace('        if display_width(&candidate) > width {', '        if candidate.chars().count() > width {')"
  "truncation_may_end_on_a_zero_width_char|${STYLE}|s.replace('''    while out\n        .chars()\n        .next_back()\n        .is_some_and(|c| display_width(&out) == display_width(&out[..out.len() - c.len_utf8()]))\n    {\n        out.pop();\n    }\n''', '')"
  # The minimum-width constant must stay DERIVED from the widest marker. Written
  # down as 24 it silently excluded every zoomed pane.
  "min_boxable_width_forgets_the_zoomed_marker|${STYLE}|s.replace('    2 + 8 + GAP + display_width(&pane_marker(true, true)) + 2', '    24')"
  # NOT a mutation: swapping the guard and the quoting is provably equivalent
  # (every character `safe_label` rewrites already fails the quoting allowlist;
  # 200,000 random argvs, zero differences), so it is an equivalent mutant and
  # counting it as a survivor would be counting a kill nobody can earn. What IS
  # observable is dropping the guard, on either field:
  "egress_guard_dropped_from_the_command|${STYLE}|s.replace('safe_label(&render_argv(&p.command)),', 'render_argv(&p.command),')"
  "egress_guard_dropped_from_the_title|${STYLE}|s.replace('                    safe_label(&p.title),\n                );', '                    p.title.clone(),\n                );')"
  "quoting_back_to_the_denylist|${PTY}|s.replace('''    let unquoted_is_literal = arg.bytes().all(|b| {\n        b.is_ascii_alphanumeric()\n            || matches!(\n                b,\n                b'_' | b'-' | b'.' | b'/' | b',' | b':' | b'=' | b'+' | b'@' | b'%'\n            )\n    });''', '''    let unquoted_is_literal = !(arg.contains(' ') || arg.contains('\\\\'') || arg.contains('\$'));''')"
  "empty_argument_not_quoted|${PTY}|s.replace('''    if arg.is_empty() {\n        return \"''\".to_string();\n    }''', '')"
  "leading_equals_not_quoted|${PTY}|s.replace(\"    if arg.starts_with('=') {\", '    if false {')"
  # A pane whose zoom is invisible in both human formats while `--format json`
  # reports it.
  "zoom_hidden_without_focus|${STYLE}|s.replace('(false, true) => \"[zoomed]\".to_string(),', '(false, true) => String::new(),')"
  # `args` is TYPED INTO A TERMINAL. Correct quoting put the control byte inside
  # the quotes, so the line discipline's truncation left an unterminated quote
  # and wedged the pane for good.
  "control_bytes_allowed_into_a_typed_line|${PC}|s.replace(\"(*c as u32) < 0x20 || *c == '\\\\u{7f}'\", 'false')"
  "only_the_three_signal_bytes_rejected|${PC}|s.replace(\"(*c as u32) < 0x20 || *c == '\\\\u{7f}'\", \"matches!(*c, '\\\\u{3}' | '\\\\u{15}' | '\\\\u{1a}')\")"
)

printf '%-46s %s\n' "MUTATION" "KILLED BY"
printf '%-46s %s\n' "----------------------------------------------" "---------"

survivors=()
for entry in "${MUTATIONS[@]}"; do
  name="${entry%%|*}"
  rest="${entry#*|}"
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
    printf '%-46s %s\n' "${name}" "ANCHOR MISSED"
    survivors+=("${name} (anchor missed)")
    continue
  fi

  # Which suite owns this file. `--color never` is pinned at the call site: CI
  # exports CARGO_TERM_COLOR=always, and the `sed` below anchors on plain text.
  case "${file}" in
    "${PTY}") log="$(cargo test --color never -p shux-pty --lib 2>&1 || true)" ;;
    "${PC}") log="$(cargo test --color never -p shux --bin shux -- pane_command::tests 2>&1 || true)" ;;
    *) log="$(cargo test --color never -p shux --bin shux -- style::tests 2>&1 || true)" ;;
  esac

  killers="$(printf '%s\n' "${log}" | sed -n 's/^    \([a-z_:]*\)$/\1/p' | tr '\n' ' ')"
  if [ -z "${killers}" ]; then
    # Compile failure counts as a survivor: a mutation must be a behaviour
    # change the tests catch, not a syntax error.
    if printf '%s\n' "${log}" | grep -q '^error'; then
      printf '%-46s %s\n' "${name}" "DID NOT COMPILE"
      survivors+=("${name} (did not compile)")
    else
      printf '%-46s %s\n' "${name}" "SURVIVED"
      survivors+=("${name}")
    fi
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
