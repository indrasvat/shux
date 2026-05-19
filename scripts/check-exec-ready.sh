#!/usr/bin/env bash
set -euo pipefail

# nextest discovers tests by executing each test binary with
# `--list --format terse`. On macOS, local security policy failures can leave
# freshly built Mach-O processes parked in dyld before Rust test code runs.
# Catch that host-level failure before cargo-nextest wedges in discovery.

if [[ "$(uname -s)" != "Darwin" ]]; then
  exit 0
fi

timeout_seconds="${SHUX_EXEC_PREFLIGHT_TIMEOUT_SECONDS:-8}"
tmpdir="$(mktemp -d "${TMPDIR:-/tmp}/shux-exec-check.XXXXXX")"

cleanup() {
  rm -rf "${tmpdir}"
}
trap cleanup EXIT

probe_c="${tmpdir}/probe.c"
probe_bin="${tmpdir}/probe"

cat >"${probe_c}" <<'C'
#include <stdio.h>

int main(void) {
    puts("ok");
    return 0;
}
C

if ! cc "${probe_c}" -o "${probe_bin}" >/dev/null 2>&1; then
  echo "error: failed to compile local exec preflight probe with cc" >&2
  echo "hint: install/repair the Xcode command-line tools, then rerun make test" >&2
  exit 1
fi

set +e
output="$(perl -e 'alarm shift @ARGV; exec @ARGV' "${timeout_seconds}" "${probe_bin}" 2>&1)"
status=$?
set -e

if [[ "${status}" -eq 0 && "${output}" == "ok" ]]; then
  exit 0
fi

cat >&2 <<EOF
error: macOS cannot execute freshly built local binaries in this shell.

nextest discovers tests by launching each test binary with --list. When the
host blocks those unsigned binaries before main(), nextest waits in discovery
and make/pre-push appear hung.

observed: probe exit=${status}, output=${output:-<none>}

Try enabling Developer Tools access for the terminal app running this session
or clearing/restarting the local security-policy state, then rerun:

  make test

To bypass only this preflight after fixing the host state:

  SHUX_EXEC_PREFLIGHT=0 make test
EOF

exit 124
