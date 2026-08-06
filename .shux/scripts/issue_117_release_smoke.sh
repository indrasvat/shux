#!/usr/bin/env bash
# Post-merge smoke for issue #117, against the PUBLISHED binary.
#
# CLAUDE.md feature protocol step 12: after merge and semantic-release tagging,
# install via the real `curl | sh` path and smoke the binary a user actually
# gets — not the one sitting in ./target from the branch that was just merged.
#
#   .shux/scripts/issue_117_release_smoke.sh [expected-version]
#
# What it checks, in order, because a later step is meaningless if an earlier
# one lied:
#   1. the install script runs and produces a binary
#   2. that binary is NEWER than the release the fix was branched from
#      (v0.46.7) — otherwise the release did not pick up the merge and every
#      behavioural check below would be testing the OLD code and passing
#   3. DECALN actually fills a real pane, end to end
#   4. `?47` enters the alternate screen and gives the primary back
#      (the second fix in the same release)
#   5. no daemon is left behind
#
# Nothing here is allowed to swallow a failure.

set -uo pipefail

BASE_RELEASE="v0.46.7"   # the release #117 was branched from
expected="${1:-}"
work="$(mktemp -d "${TMPDIR:-/tmp}/shux-117-smoke.XXXXXX")"
runtime="${work}/rt"
mkdir -p "${runtime}"
failures=0

note() { printf '    %s\n' "$*"; }
pass() { printf '    ok   %s\n' "$*"; }
fail() { printf '    FAIL %s\n' "$*"; failures=$((failures + 1)); }

cleanup() {
  if [ -n "${BIN:-}" ] && [ -x "${BIN}" ]; then
    env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${BIN}" daemon stop >/dev/null 2>&1
  fi
  sleep 1
  local pidfile="${runtime}/shux/shux.pid"
  if [ -f "${pidfile}" ]; then
    local pid; pid="$(cat "${pidfile}" 2>/dev/null || true)"
    if [ -n "${pid}" ] && kill -0 "${pid}" 2>/dev/null; then
      printf '    FAIL daemon %s outlived the smoke\n' "${pid}"
      kill -KILL "${pid}" 2>/dev/null || true
      failures=$((failures + 1))
    fi
  fi
  rm -rf "${work}"
}
trap cleanup EXIT

# ── 1. install the way a user does ──────────────────────────────────────
echo "==> installing from https://shux.pages.dev/install.sh"
export SHUX_INSTALL_DIR="${work}/bin"
mkdir -p "${SHUX_INSTALL_DIR}"
if ! curl -fsSL https://shux.pages.dev/install.sh | sh > "${work}/install.log" 2>&1; then
  echo "FATAL: install script failed" >&2
  sed 's/^/    /' "${work}/install.log" >&2
  exit 1
fi
BIN="$(command -v shux 2>/dev/null || true)"
[ -x "${SHUX_INSTALL_DIR}/shux" ] && BIN="${SHUX_INSTALL_DIR}/shux"
if [ -z "${BIN}" ] || [ ! -x "${BIN}" ]; then
  echo "FATAL: install produced no runnable binary" >&2
  sed 's/^/    /' "${work}/install.log" >&2
  exit 1
fi
note "binary: ${BIN}"

# ── 2. is it actually the NEW release? ──────────────────────────────────
ver_line="$("${BIN}" version 2>&1 | head -1)"
note "reports: ${ver_line}"
got="$(printf '%s' "${ver_line}" | grep -o '[0-9]\+\.[0-9]\+\.[0-9]\+' | head -1)"
if [ -z "${got}" ]; then
  fail "could not parse a version out of: ${ver_line}"
elif [ "v${got}" = "${BASE_RELEASE}" ]; then
  fail "published binary is still ${BASE_RELEASE} — the release did not pick up the merge, so every check below would be testing the OLD code"
  echo "==> aborting: a green result here would be a lie" >&2
  exit 1
elif [ -n "${expected}" ] && [ "v${got}" != "${expected#v}" ] && [ "v${got}" != "v${expected#v}" ]; then
  fail "expected ${expected}, got v${got}"
else
  pass "version advanced past ${BASE_RELEASE} (v${got})"
fi

sx() { env -u SHUX_SOCKET XDG_RUNTIME_DIR="${runtime}" "${BIN}" "$@"; }

# ── 3. DECALN through a real pane ───────────────────────────────────────
echo "==> DECALN through a real pane"
cat >"${work}/decaln.sh" <<'INNER'
#!/bin/sh
while [ ! -e "$GO" ]; do sleep 0.05; done
printf '\033[38;2;120;220;180mTRUECOLOR\033[0m \033[38;5;208mINDEXED\033[0m \033[34mBASIC\033[0m\n'
printf '\033#8'
: > "$DONE"
exec sleep 300
INNER
sx session create smoke117 -d -- env GO="${work}/go" DONE="${work}/done" \
  TERM=xterm-256color COLORTERM=truecolor sh "${work}/decaln.sh" >/dev/null 2>&1
pane="$(sx --format json pane list -s smoke117 2>/dev/null | jq -r '.[0].id')"
if [ -z "${pane}" ] || [ "${pane}" = "null" ]; then
  fail "could not create a pane"
else
  sx pane set-size -s smoke117 -p "${pane}" --cols 40 --rows 10 >/dev/null
  : > "${work}/go"
  deadline=$((SECONDS + 30))
  while [ ! -e "${work}/done" ]; do
    [ "${SECONDS}" -ge "${deadline}" ] && break
    sleep 0.1
  done
  sx pane wait-settled "${pane}" --quiet 400 --timeout 15000 >/dev/null 2>&1
  text="$(sx pane capture -s smoke117 -p "${pane}" --lines 10)"
  full="$(printf 'E%.0s' $(seq 1 40))"
  rows="$(printf '%s\n' "${text}" | grep -c -- "${full}")"
  if [ "${rows}" = "10" ]; then
    pass "all 10 rows filled with the alignment pattern"
  else
    fail "expected 10 filled rows, got ${rows}"
    printf '%s\n' "${text}" | head -4 | sed 's/^/         /'
  fi
  sx session kill smoke117 >/dev/null 2>&1
fi

# ── 4. ?47 — the other fix riding this release ──────────────────────────
echo "==> ESC[?47h round trip"
cat >"${work}/m47.sh" <<'INNER'
#!/bin/sh
while [ ! -e "$GO" ]; do sleep 0.05; done
printf 'PRIMARY-SURVIVES\n'
printf '\033[?47h\033#8'
printf '\033[?47l'
: > "$DONE"
exec sleep 300
INNER
rm -f "${work}/go" "${work}/done"
sx session create smoke47 -d -- env GO="${work}/go" DONE="${work}/done" \
  TERM=xterm-256color sh "${work}/m47.sh" >/dev/null 2>&1
pane47="$(sx --format json pane list -s smoke47 2>/dev/null | jq -r '.[0].id')"
if [ -z "${pane47}" ] || [ "${pane47}" = "null" ]; then
  fail "could not create the ?47 pane"
else
  sx pane set-size -s smoke47 -p "${pane47}" --cols 40 --rows 10 >/dev/null
  : > "${work}/go"
  deadline=$((SECONDS + 30))
  while [ ! -e "${work}/done" ]; do
    [ "${SECONDS}" -ge "${deadline}" ] && break
    sleep 0.1
  done
  sx pane wait-settled "${pane47}" --quiet 400 --timeout 15000 >/dev/null 2>&1
  t47="$(sx pane capture -s smoke47 -p "${pane47}" --lines 10)"
  if printf '%s\n' "${t47}" | grep -q "PRIMARY-SURVIVES"; then
    if printf '%s\n' "${t47}" | grep -q "EEEE"; then
      fail "?47 left the pattern on the primary screen"
    else
      pass "?47 took the alternate screen and gave the primary back intact"
    fi
  else
    fail "?47 destroyed the primary screen"
    printf '%s\n' "${t47}" | head -4 | sed 's/^/         /'
  fi
  sx session kill smoke47 >/dev/null 2>&1
fi

echo "==> ${failures} failure(s)"
[ "${failures}" -eq 0 ]
