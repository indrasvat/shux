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
# `border_style = "none"` is not a nicety: the pane viewport rule differs
# there, and a snapshot path frozen on `Rounded` while the live compositor
# followed the config cropped every pane's last two columns and rows out of
# the image. That shows up here as an A/B divergence and nowhere else.
border_style="${BORDER_STYLE:-rounded}"
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

# Runtime dirs are tracked in a global so the EXIT trap can tear down a daemon
# whichever line `set -e` aborted on. Without this, a `wait-for` timeout left a
# live daemon behind -- reproduced, and exactly what "zero leaked daemons" is
# there to stop.
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

# render <label> <binary> -> $out_dir/<label>.png + .txt
render() {
  local label="$1" bin="$2"
  local runtime; runtime="$(mktemp -d "${TMPDIR:-/tmp}/shux-174-px-${label}.XXXXXX")"
  runtimes+=("${runtime}")
  mkdir -p "${runtime}/config/shux"
  printf '[appearance]\nborder_style = "%s"\n' "${border_style}" \
    >"${runtime}/config/shux/config.toml"
  # SAME name on both sides: it is drawn in the window title and the status
  # bar, so `px174-base` vs `px174-head` made every window comparison differ on
  # text that has nothing to do with the change. The two runs are sequential and
  # in separate runtime dirs, so the name cannot collide.
  local session="px174"
  local script="${runtime}/pane.sh"
  { printf '%s\n' "${payload}"; printf 'sleep 120\n'; } >"${script}"

  env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" XDG_CONFIG_HOME="${runtime}/config" "${bin}" session create "${session}" -d \
    --title "${session}" -- \
    env TERM=xterm-256color COLORTERM=truecolor LANG=C.utf8 LC_ALL=C.utf8 \
        HOME="${runtime}" sh "${script}" >/dev/null
  local pane
  pane="$(env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" XDG_CONFIG_HOME="${runtime}/config" "${bin}" --format json pane list \
    -s "${session}" | python3 -c 'import json,sys; print(json.load(sys.stdin)[0]["id"])')"
  env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" XDG_CONFIG_HOME="${runtime}/config" "${bin}" pane set-size -s "${session}" \
    -p "${pane}" --cols "${cols}" --rows "${rows}" >/dev/null
  env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" XDG_CONFIG_HOME="${runtime}/config" "${bin}" pane wait-for -s "${session}" \
    -p "${pane}" -t RENDERED --timeout-ms 20000 >/dev/null
  env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" XDG_CONFIG_HOME="${runtime}/config" "${bin}" pane wait-settled "${pane}" \
    --quiet 250 --timeout 8000 >/dev/null 2>&1 || true
  env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" XDG_CONFIG_HOME="${runtime}/config" "${bin}" pane capture -s "${session}" \
    -p "${pane}" --lines "${rows}" >"${out_dir}/${label}.txt"
  env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" XDG_CONFIG_HOME="${runtime}/config" "${bin}" pane snapshot -s "${session}" \
    -p "${pane}" -o "${out_dir}/${label}.png" >/dev/null
  # The WINDOW composer too -- it is the path that was frozen on `Rounded`.
  env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" XDG_CONFIG_HOME="${runtime}/config" "${bin}" \
    window snapshot -s "${session}" -o "${out_dir}/${label}-window.png" >/dev/null

  env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" XDG_CONFIG_HOME="${runtime}/config" "${bin}" session kill "${session}" \
    >/dev/null 2>&1 || true
  # Teardown is the EXIT trap's job -- see `runtimes` above.
}

echo "==> pixel A/B (border_style=${border_style}): base $(${base_bin} version 2>/dev/null | head -1)"
echo "               head $(${shux_bin} version 2>/dev/null | head -1)"

render base "${base_bin}"
render head "${shux_bin}"

# Two blank screens also compare equal, so BOTH sides must be shown to carry
# content -- and the check has to look at the PNGs, not only the text. A
# rasterizer regression that renders blank on both trees leaves the text
# captures full of TRUECOLOR while the images say nothing, and the comparison
# then reports a confident, meaningless 0.
for label in base head; do
  for needle in TRUECOLOR INDEXED BASIC RENDERED; do
    if ! grep -q -- "${needle}" "${out_dir}/${label}.txt"; then
      echo "    FAIL — ${label} capture is missing ${needle}; nothing to compare"
      exit 1
    fi
  done
done
uv run --script "${repo_root}/.shux/scripts/lib/png_not_blank.py" \
  "${out_dir}/base.png" "${out_dir}/head.png" \
  "${out_dir}/base-window.png" "${out_dir}/head-window.png" \
  --min-colors 8 --min-ink-ratio 0.01

compare() {
  local what="$1" suffix="$2"
  local metric="${metric_out}/pixel-render-parity-${border_style}-${what}.json"
  uv run --script "${repo_root}/.claude/automations/pixel_verify.py" \
    "${out_dir}/head${suffix}.png" "${out_dir}/base${suffix}.png" \
    --diff "${out_dir}/render-parity-${border_style}-${what}-diff.png" \
    --max-pixel-diff-ratio 0 --max-mean-channel-delta 0 >"${metric}"
  printf '    %-7s ' "${what}"
  python3 - "${metric}" <<'PY'
import json, sys
m = json.load(open(sys.argv[1]))
keys = ("status", "changed_pixels", "total_pixels", "pixel_diff_ratio", "mean_rgba_channel_delta")
missing = [k for k in keys if k not in m]
if missing:
    print(f"FAIL — comparator did not report {missing}; its output shape changed")
    sys.exit(1)
print(json.dumps({k: m[k] for k in keys}))
if m.get("status") != "pass":
    print("FAIL — the rendered output changed; this change should move no pixel")
    sys.exit(1)
PY
}

# Both the pane rasterizer and the WINDOW composer. The window path is the one
# that was frozen on `Rounded` while the live compositor followed the config.
#
# The parity claim holds under the DEFAULT style only, and that is the honest
# scope: under `border_style = "none"` the window snapshot is SUPPOSED to
# differ, because base ignored the setting and drew rounded borders anyway.
# `issue_174_snapshot_style_check.sh` is where that direction is asserted.
compare pane ""
if [ "${border_style}" = "rounded" ]; then
  compare window "-window"
else
  echo "    window  skipped — base ignores border_style here; see"
  echo "            issue_174_snapshot_style_check.sh for that direction"
fi

echo "    metrics: ${metric_out}"
echo "==> PASS — byte-identical rendering (border_style=${border_style})"
