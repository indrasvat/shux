#!/usr/bin/env bash
# Issue #174: prove the change alters no rendered pixel.
#
# Neither half of this change is supposed to be visible. Half A alters what a
# pane child is TOLD about its geometry; half B alters what it RECEIVES from the
# mouse. Neither touches a glyph. The claim "no rendering regression" is worth
# nothing as prose, so it is measured: the same deterministic pane content is
# rasterized by the base binary and by this one, and the two PNGs are compared
# at exact thresholds (0 differing pixels, 0 mean channel delta).
#
#   BASE_BIN=<base binary> SHUX_BIN=<branch binary> \
#     .shux/scripts/issue_174_pixel_ab.sh
#
# The content is colour-probed (truecolor + indexed + basic) and includes box
# drawing and wide characters, so a monochrome or width regression cannot pass
# as "identical" by being identically blank. The screen is asserted non-empty
# before either PNG is trusted -- two blank images also compare equal.
#
# Output: .shux/out/issue-174/pixel/ (scratch) and the metric JSON the VT gate
# consumes.

set -euo pipefail

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
source "${repo_root}/.shux/scripts/lib/shux_harness.sh"

shux_bin="${SHUX_BIN:-${repo_root}/target/debug/shux}"
base_bin="${BASE_BIN:?BASE_BIN must point at a binary built from the base commit}"
out_dir="${repo_root}/.shux/out/issue-174/pixel"
metric_out="${METRIC_OUT:-${out_dir}}"
cols="${EVID_COLS:-100}"
rows="${EVID_ROWS:-30}"

mkdir -p "${out_dir}" "${metric_out}"

# Deterministic, non-blank, colour-probed, and exercising the cell shapes a
# width or attribute regression would move: box drawing, a wide CJK run, and a
# combining sequence.
payload=$(cat <<'EOF'
printf '\033[38;2;120;220;180mTRUECOLOR\033[0m \033[38;5;208mINDEXED\033[0m \033[34mBASIC\033[0m\n'
printf '\033[1mBOLD\033[0m \033[3mITALIC\033[0m \033[4mUNDER\033[0m \033[7mREVERSE\033[0m \033[9mSTRIKE\033[0m\n'
printf '\342\224\214\342\224\200\342\224\200\342\224\254\342\224\200\342\224\200\342\224\220 box\n'
printf '\346\227\245\346\234\254\350\252\236 wide  e\314\201 combining\n'
printf '\033[48;2;40;40;90m\033[38;2;255;210;90m bg+fg truecolor \033[0m\n'
for i in 1 2 3 4 5 6; do printf 'row %d: \033[3%dmcolour\033[0m 0123456789\n' "$i" "$i"; done
printf 'RENDERED\n'
EOF
)

# render <label> <binary> -> $out_dir/<label>.png + .txt
render() {
  local label="$1" bin="$2"
  local runtime; runtime="$(mktemp -d "${TMPDIR:-/tmp}/shux-174-px-${label}.XXXXXX")"
  local session="px174-${label}"
  local script="${runtime}/pane.sh"
  { printf '%s\n' "${payload}"; printf 'sleep 120\n'; } >"${script}"

  env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${bin}" session create "${session}" -d \
    --title "${session}" -- \
    env TERM=xterm-256color COLORTERM=truecolor LANG=C.utf8 LC_ALL=C.utf8 \
        HOME="${runtime}" sh "${script}" >/dev/null
  local pane
  pane="$(env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${bin}" --format json pane list \
    -s "${session}" | python3 -c 'import json,sys; print(json.load(sys.stdin)[0]["id"])')"
  env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${bin}" pane set-size -s "${session}" \
    -p "${pane}" --cols "${cols}" --rows "${rows}" >/dev/null
  env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${bin}" pane wait-for -s "${session}" \
    -p "${pane}" -t RENDERED --timeout-ms 20000 >/dev/null
  env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${bin}" pane wait-settled "${pane}" \
    --quiet 250 --timeout 8000 >/dev/null 2>&1 || true
  env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${bin}" pane capture -s "${session}" \
    -p "${pane}" --lines "${rows}" >"${out_dir}/${label}.txt"
  env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${bin}" pane snapshot -s "${session}" \
    -p "${pane}" -o "${out_dir}/${label}.png" >/dev/null

  env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${bin}" session kill "${session}" \
    >/dev/null 2>&1 || true
  shux_harness_stop_daemon "${runtime}"
  shux_harness_assert_no_daemon "${runtime}" || shux_harness_stop_daemon "${runtime}"
  sleep 0.3
  rm -rf "${runtime}"
}

echo "==> pixel A/B: base $(${base_bin} version 2>/dev/null | head -1)"
echo "               head $(${shux_bin} version 2>/dev/null | head -1)"

render base "${base_bin}"
render head "${shux_bin}"

# Two blank screens also compare equal. Require content on BOTH before the
# comparison is allowed to mean anything.
for label in base head; do
  for needle in TRUECOLOR INDEXED BASIC RENDERED; do
    if ! grep -q -- "${needle}" "${out_dir}/${label}.txt"; then
      echo "    FAIL — ${label} capture is missing ${needle}; nothing to compare"
      exit 1
    fi
  done
done

metric="${metric_out}/pixel-render-parity.json"
uv run --script "${repo_root}/.claude/automations/pixel_verify.py" \
  "${out_dir}/head.png" "${out_dir}/base.png" \
  --diff "${out_dir}/render-parity-diff.png" \
  --max-pixel-diff-ratio 0 --max-mean-channel-delta 0 >"${metric}"

python3 - "${metric}" <<'PY'
import json, sys
m = json.load(open(sys.argv[1]))
print("    " + json.dumps({k: m[k] for k in ("status", "diff_pixels", "mean_channel_delta") if k in m}))
if m.get("status") != "pass":
    print("    FAIL — the rendered output changed; this change should move no pixel")
    sys.exit(1)
PY

echo "    metric: ${metric}"
echo "==> PASS — byte-identical rendering across the change"
