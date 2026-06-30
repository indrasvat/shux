#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
usage: install-sightline.sh [--ref REF] [--dest DIR]

Downloads the minimal Sightline plugin package without cloning the shux repo.
Default destination:
  ${SHUX_PLUGIN_CACHE_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/shux/plugins}/sightline/<ref>
USAGE
}

ref="${SHUX_SIGHTLINE_REF:-main}"
dest=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --ref)
      [[ $# -ge 2 ]] || { echo "--ref requires a value" >&2; exit 2; }
      ref="$2"
      shift 2
      ;;
    --dest)
      [[ $# -ge 2 ]] || { echo "--dest requires a value" >&2; exit 2; }
      dest="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
skill_dir="$(cd "${script_dir}/.." && pwd -P)"
repo_root="$(cd "${skill_dir}/../.." 2>/dev/null && pwd -P || true)"
local_pkg="${repo_root}/plugins/sightline"
cache_root="${SHUX_PLUGIN_CACHE_DIR:-${XDG_CACHE_HOME:-${HOME}/.cache}/shux/plugins}"
ref_key="$(printf '%s' "${ref}" | LC_ALL=C sed 's/[^A-Za-z0-9._-]/_/g')"
if [[ -z "${ref_key}" ]]; then
  ref_key="main"
fi
if [[ -z "${dest}" ]]; then
  dest="${cache_root}/sightline/${ref_key}"
fi

mkdir -p "${dest}/bin"

if [[ -f "${local_pkg}/shux-plugin.toml" && -f "${local_pkg}/bin/sightline" ]]; then
  cp "${local_pkg}/shux-plugin.toml" "${dest}/shux-plugin.toml"
  cp "${local_pkg}/bin/sightline" "${dest}/bin/sightline"
  if [[ -f "${local_pkg}/README.md" ]]; then
    cp "${local_pkg}/README.md" "${dest}/README.md"
  fi
else
  raw_base="https://raw.githubusercontent.com/indrasvat/shux/${ref}/plugins/sightline"
  command -v curl >/dev/null 2>&1 || { echo "curl is required" >&2; exit 1; }
  curl -fsSL "${raw_base}/shux-plugin.toml" -o "${dest}/shux-plugin.toml"
  curl -fsSL "${raw_base}/bin/sightline" -o "${dest}/bin/sightline"
  curl -fsSL "${raw_base}/README.md" -o "${dest}/README.md"
fi

chmod +x "${dest}/bin/sightline"

if [[ ! -f ".shux/.gitignore" ]]; then
  echo "note: run 'shux init' in target projects so Sightline evidence under .shux/out is gitignored" >&2
fi

cat <<EOF
Sightline installed at: ${dest}
Runner:
  ${dest}/bin/sightline verify --session <name> --pane <pane-id>

Reusable shell variable:
  SIGHTLINE_RUNNER="${dest}/bin/sightline"

Optional plugin lifecycle smoke:
  shux plugin install ${dest} --no-watch
  shux plugin stop sightline
EOF
