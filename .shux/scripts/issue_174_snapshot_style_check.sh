#!/usr/bin/env bash
# Issue #174: `window.snapshot` / `session.snapshot` must honour
# `appearance.border_style`.
#
# The snapshot composer was frozen on `BorderStyle::Rounded`. Two bugs sat on
# that one argument:
#
#   · cosmetic and long-standing -- a user configured `thick` / `ascii` /
#     `double` / `none` got rounded borders in every snapshot PNG;
#   · not cosmetic -- `compose` derives the pane viewport from the style, so
#     once a pane's PTY started following the LIVE compositor's rule, a snapshot
#     under `border_style = "none"` composed panes into rects two columns and
#     two rows smaller than their grids and silently CROPPED the right and
#     bottom edges out of the image.
#
#   SHUX_BIN=<binary> [BASE_BIN=<base binary>] \
#     .shux/scripts/issue_174_snapshot_style_check.sh
#
# The assertion is a difference, not a likeness: the same window rendered under
# `none` and under `rounded` must produce DIFFERENT pixels. A binary that
# ignores the setting produces identical ones, which is exactly what `BASE_BIN`
# is here to demonstrate -- run it and the check reports the old behaviour, so
# the new behaviour is not being asserted against itself.
#
# Output: .shux/out/issue-174/snapstyle/. Gitignored scratch.

set -euo pipefail

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
source "${repo_root}/.shux/scripts/lib/shux_harness.sh"

shux_bin="${SHUX_BIN:-${repo_root}/target/debug/shux}"
out_dir="${repo_root}/.shux/out/issue-174/snapstyle"
cols="${EVID_COLS:-100}"
rows="${EVID_ROWS:-30}"

runtimes=()
cleanup() {
  local rt
  for rt in "${runtimes[@]:-}"; do
    [ -n "${rt}" ] || continue
    shux_harness_stop_daemon "${rt}"
    shux_harness_assert_no_daemon "${rt}" || shux_harness_stop_daemon "${rt}"
    rm -rf "${rt}"
  done
}
trap cleanup EXIT

mkdir -p "${out_dir}"

# render <bin> <style> <outfile>
render() {
  local bin="$1" style="$2" outfile="$3"
  local runtime; runtime="$(mktemp -d "${TMPDIR:-/tmp}/shux-174-ss.XXXXXX")"
  runtimes+=("${runtime}")
  mkdir -p "${runtime}/config/shux"
  printf '[appearance]\nborder_style = "%s"\n' "${style}" \
    >"${runtime}/config/shux/config.toml"
  local script="${runtime}/pane.sh"
  {
    printf "printf '\\033[38;2;120;220;180mTRUECOLOR\\033[0m \\033[38;5;208mINDEXED\\033[0m \\033[34mBASIC\\033[0m\\n'\n"
    # Fill every column of every row, so a viewport that is two cells too small
    # crops visible content rather than blank space.
    #
    # shellcheck disable=SC2016  # `$i` and `$(...)` are for the PANE's shell to
    # expand when it runs this generated script, not for this one. Only `%s`
    # (the column count) is substituted here, which is why it is `printf`.
    printf 'i=1; while [ "$i" -le 20 ]; do printf "\\033[48;2;200;40;40m%%s\\033[0m\\n" "$(printf "X%%.0s" $(seq 1 %s))"; i=$((i+1)); done\n' "${cols}"
    printf "printf 'RENDERED\\n'\n"
    printf 'sleep 120\n'
  } >"${script}"

  sx() { env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" \
      XDG_CONFIG_HOME="${runtime}/config" "${bin}" "$@"; }
  sx session create snapstyle -d --title snapstyle -- \
    env TERM=xterm-256color COLORTERM=truecolor LANG=C.utf8 LC_ALL=C.utf8 \
        HOME="${runtime}" sh "${script}" >/dev/null
  local pane
  pane="$(sx --format json pane list -s snapstyle \
    | python3 -c 'import json,sys; print(json.load(sys.stdin)[0]["id"])')"
  sx pane set-size -s snapstyle -p "${pane}" --cols "${cols}" --rows "${rows}" >/dev/null
  sx pane wait-for -s snapstyle -p "${pane}" -t RENDERED --timeout-ms 20000 >/dev/null
  sx pane wait-settled "${pane}" --quiet 250 --timeout 8000 >/dev/null 2>&1 || true
  sx pane capture -s snapstyle -p "${pane}" --lines "${rows}" >"${outfile%.png}.txt"
  sx window snapshot -s snapstyle -o "${outfile}" >/dev/null
  sx session kill snapstyle >/dev/null 2>&1 || true
  grep -q TRUECOLOR "${outfile%.png}.txt" \
    || { echo "    FAIL — colour probe missing from ${outfile}"; exit 1; }
}

failures=0

# same <a> <b> -> 0 when byte-identical pixels
same() {
  uv run --script "${repo_root}/.claude/automations/pixel_verify.py" "$1" "$2" \
    --max-pixel-diff-ratio 0 --max-mean-channel-delta 0 \
    | python3 -c 'import json,sys; sys.exit(0 if json.load(sys.stdin)["status"]=="pass" else 1)'
}

echo "==> snapshots honour appearance.border_style: $(${shux_bin} version 2>/dev/null | head -1)"

render "${shux_bin}" none    "${out_dir}/head-none.png"
render "${shux_bin}" rounded "${out_dir}/head-rounded.png"
uv run --script "${repo_root}/.shux/scripts/lib/png_not_blank.py" \
  "${out_dir}/head-none.png" "${out_dir}/head-rounded.png" \
  --min-colors 8 --min-ink-ratio 0.01

if same "${out_dir}/head-none.png" "${out_dir}/head-rounded.png"; then
  echo "    FAIL — 'none' and 'rounded' rendered identically; the setting is ignored"
  failures=$((failures + 1))
else
  echo "    ok   'none' and 'rounded' render differently — the setting is honoured"
fi

# The control. Without it, "they differ" is asserted against nothing: this shows
# the check is capable of reporting the OLD behaviour when handed a binary that
# has it.
if [ -n "${BASE_BIN:-}" ]; then
  render "${BASE_BIN}" none    "${out_dir}/base-none.png"
  render "${BASE_BIN}" rounded "${out_dir}/base-rounded.png"
  if same "${out_dir}/base-none.png" "${out_dir}/base-rounded.png"; then
    echo "    ok   control: the base binary ignores the setting (identical) — the defect"
  else
    echo "    FAIL — control: the base binary already honoured the setting;"
    echo "           this check is not measuring what it claims"
    failures=$((failures + 1))
  fi
else
  echo "    ..   control skipped — set BASE_BIN to demonstrate the old behaviour"
fi

echo "    artifacts: ${out_dir}"
if [ "${failures}" -ne 0 ]; then
  echo "==> FAIL (${failures})"
  exit 1
fi
echo "==> PASS"
