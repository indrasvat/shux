#!/usr/bin/env bash
# Two REAL builds against each other (#318): a daemon of one build holding a
# live session, the CLI of another, in both directions, plus the upgrade
# handover and an install replaced under a running daemon.
#
#   NEW   this tree's binary                       (required)
#   PEER  this tree built as an older release      (required; `make build-skew-peer`)
#   PREV  shipped releases, space-separated paths  (optional; CI downloads them)
#
# Each scenario has its own short runtime dir and stops its daemons. Run it
# through `make test-skew`, which wraps it in the no-leak guard.
set -euo pipefail

: "${NEW:?NEW=<path to this build>}"
: "${PEER:?PEER=<path to the peer build>}"
PREV="${PREV:-}"

OUT="$(mktemp -d /tmp/skw-out.XXXXXX)"
RUNTIMES=()
fails=0

step() { printf '\033[1;36m▶ %s\033[0m\n' "$*"; }
pass() { printf '  \033[32m✓ %s\033[0m\n' "$*"; }
fail() {
  printf '  \033[1;31m✗ %s\033[0m\n' "$*"
  fails=$((fails + 1))
}

cleanup() {
  local rt
  for rt in "${RUNTIMES[@]}"; do
    XDG_RUNTIME_DIR="$rt" "$NEW" daemon stop >/dev/null 2>&1 || true
    rm -rf "$rt"
  done
  rm -rf "$OUT"
}
trap cleanup EXIT

# new_runtime: sets `rt` to a fresh short runtime dir that cleanup stops.
# Called bare, never in `$(...)`: a subshell would drop the record cleanup reads.
new_runtime() {
  rt="$(mktemp -d /tmp/skw.XXXXXX)"
  RUNTIMES+=("$rt")
}

# expect <exit> <label> <bin> <runtime> <args...>: run, check the exit code,
# keep stdout and stderr in $OUT/out and $OUT/err.
expect() {
  local want="$1" label="$2" bin="$3" rt="$4"
  shift 4
  local status=0
  XDG_RUNTIME_DIR="$rt" NO_COLOR=1 SHELL=/bin/sh "$bin" "$@" >"$OUT/out" 2>"$OUT/err" || status=$?
  if [ "$status" -eq "$want" ]; then
    pass "$label (exit $status)"
  else
    fail "$label: exit $status, wanted $want"
    sed 's/^/      /' "$OUT/out" "$OUT/err"
  fi
}

# contains <label> <needle>: the last command's stdout+stderr holds it.
contains() {
  if grep -qF -- "$2" "$OUT/out" "$OUT/err"; then
    pass "$1"
  else
    fail "$1: no \"$2\" in its output"
    sed 's/^/      /' "$OUT/out" "$OUT/err"
  fi
}

daemon_pid() { cat "$1/shux/shux.pid" 2>/dev/null || true; }

# wait_gone <pid>: until that process exits, for at most 30s.
wait_gone() {
  local pid="$1" deadline=$((SECONDS + 30))
  while kill -0 "$pid" 2>/dev/null; do
    if [ "$SECONDS" -ge "$deadline" ]; then
      return 1
    fi
    sleep 0.1
  done
}

# occupy <bin> <runtime>: a daemon of <bin> holding session `agent`; its pane.
occupy() {
  XDG_RUNTIME_DIR="$2" SHELL=/bin/sh "$1" --format json session create agent -d -- /bin/sh |
    jq -r .pane_id
}

build_of() { "$1" --format json version | jq -r '"\(.cli.version) (\(.cli.git_sha))"'; }

# same_daemon <runtime> <pid> <label>: the daemon serving it is still <pid>.
same_daemon() {
  if [ "$(daemon_pid "$1")" = "$2" ]; then pass "$3"; else fail "$3: it was replaced"; fi
}

# kill_all <bin> <runtime>: end every session, so the daemon is idle.
kill_all() {
  local name
  for name in $(XDG_RUNTIME_DIR="$2" "$1" --format json session list --include-scratch | jq -r '.sessions[].name'); do
    XDG_RUNTIME_DIR="$2" "$1" session kill "$name" >/dev/null
  done
}

# Truecolor, indexed and basic colour, typed so the echo cannot hold the words.
PROBES=$'printf \'\\033[38;2;120;220;180mTRUE%sCOLOR\\033[0m \\033[38;5;208mINDE%sXED\\033[0m \\033[34mBA%sSIC\\033[0m\\n\' \'\' \'\' \'\'\n'

# colours <label> <cli> <runtime> <pane>: the probes come back in their colours.
colours() {
  local label="$1" cli="$2" rt="$3" pane="$4"
  expect 0 "$label: colour probes typed" "$cli" "$rt" pane send-keys -s agent -p "$pane" --text "$PROBES"
  expect 0 "$label: colour probes drawn" "$cli" "$rt" pane wait-for -s agent -p "$pane" --text BASIC --timeout-ms 20000
  expect 0 "$label: glance cells" "$cli" "$rt" --format json pane glance "$pane" --cells
  if jq -e '[.result.cells.rows[].runs[]] as $r
        | ([$r[] | select(.[1]=="TRUECOLOR" and .[2].fg.rgb==[120,220,180])] | length > 0)
        and ([$r[] | select(.[1]=="INDEXED" and .[2].fg.idx==208)] | length > 0)
        and ([$r[] | select(.[1]=="BASIC" and .[2].fg.idx==4)] | length > 0)' "$OUT/out" >/dev/null; then
    pass "$label: truecolor, indexed and basic colour come back"
  else
    fail "$label: a colour probe came back in the wrong colour"
  fi
}

# common <label> <cli> <runtime> <pane>: the commands agents run all day.
common() {
  local label="$1" cli="$2" rt="$3" pane="$4"
  expect 0 "$label: session list" "$cli" "$rt" session list
  contains "$label: lists the agent's session" agent
  expect 0 "$label: send-keys" "$cli" "$rt" pane send-keys -s agent -p "$pane" --text $'echo sk""ew-$((6*7))\n'
  expect 0 "$label: wait-for" "$cli" "$rt" pane wait-for -s agent -p "$pane" --text skew-42 --timeout-ms 20000
  expect 0 "$label: capture" "$cli" "$rt" pane capture -s agent -p "$pane"
  contains "$label: capture shows the output" skew-42
  colours "$label" "$cli" "$rt" "$pane"
  expect 0 "$label: pane run" "$cli" "$rt" --format json pane run -s agent -p "$pane" --boundaries markers --timeout 30s -c "printf 'r%sn\n' u"
  contains "$label: pane run returns the output" run
  expect 0 "$label: snapshot" "$cli" "$rt" --format json pane snapshot -s agent -p "$pane"
  contains "$label: snapshot is a PNG" png_base64
  expect 0 "$label: window create" "$cli" "$rt" window create -s agent --name w2 -- /bin/sh
  expect 0 "$label: session create --wait" "$cli" "$rt" --format json session create job --wait -- /bin/sh -c 'exit 7'
  contains "$label: --wait reports the job's exit" '"exit_code": 7'
}

step "builds: NEW $(build_of "$NEW"), PEER $(build_of "$PEER")"

step "1. this CLI against a busy daemon of the peer build"
new_runtime
pane="$(occupy "$PEER" "$rt")"
before="$(daemon_pid "$rt")"
common "new→peer" "$NEW" "$rt" "$pane"
expect 0 "new→peer: version" "$NEW" "$rt" --format json version
contains "new→peer: version says compatible" '"compat": "compatible"'
same_daemon "$rt" "$before" "the peer daemon was neither stopped nor replaced"

step "2. the peer CLI against a busy daemon of this build"
new_runtime
pane="$(occupy "$NEW" "$rt")"
before="$(daemon_pid "$rt")"
common "peer→new" "$PEER" "$rt" "$pane"
kill_all "$NEW" "$rt"
expect 0 "peer→new: the older CLI against the idle newer daemon" "$PEER" "$rt" session list
same_daemon "$rt" "$before" "an older CLI never replaced a newer daemon"

step "3. daemon upgrade --when-idle hands over when the last session ends"
new_runtime
occupy "$PEER" "$rt" >/dev/null
old="$(daemon_pid "$rt")"
expect 1 "upgrade without a flag refuses while busy" "$NEW" "$rt" daemon upgrade
expect 0 "upgrade --when-idle" "$NEW" "$rt" --format json daemon upgrade --when-idle
contains "it is pending" '"outcome": "pending"'
expect 0 "still serving while pending" "$NEW" "$rt" session list
expect 0 "end the last session" "$NEW" "$rt" session kill agent
if wait_gone "$old"; then pass "the old daemon retired"; else fail "the old daemon never retired"; fi
expect 0 "the next command" "$NEW" "$rt" session list
expect 0 "version" "$NEW" "$rt" --format json version
contains "the next command started this build" '"mismatch": false'

step "4. daemon upgrade --force ends the sessions now"
new_runtime
occupy "$PEER" "$rt" >/dev/null
old="$(daemon_pid "$rt")"
expect 0 "upgrade --force" "$NEW" "$rt" daemon upgrade --force
if wait_gone "$old"; then pass "the old daemon is gone"; else fail "the old daemon survived --force"; fi

step "5. the installed binary replaced under a running daemon"
new_runtime
bin_dir="$(mktemp -d /tmp/skw-bin.XXXXXX)"
RUNTIMES+=("$bin_dir")
cp "$PEER" "$bin_dir/shux"
pane="$(occupy "$bin_dir/shux" "$rt")"
old="$(daemon_pid "$rt")"
cp "$NEW" "$bin_dir/shux.new" && mv -f "$bin_dir/shux.new" "$bin_dir/shux"
common "replaced by rename" "$bin_dir/shux" "$rt" "$pane"
rm -f "$bin_dir/shux"
expect 0 "the daemon serves with the install deleted" "$NEW" "$rt" session list
same_daemon "$rt" "$old" "the same daemon, from its kept copy"

step "6. shipped releases"
if [ -z "$PREV" ]; then
  printf '  \033[33m! PREV unset: no shipped release to pair with (CI sets it)\033[0m\n'
fi
for prev in $PREV; do
  build="$(build_of "$prev")"
  new_runtime
  pane="$(occupy "$prev" "$rt")"
  answer="$(XDG_RUNTIME_DIR="$rt" "$prev" rpc call system.version --params '{}')"
  wire="$(jq -r '.result.wire // empty' <<<"$answer")"
  version="$(jq -r '.result.version' <<<"$answer")"
  if [ -n "$wire" ]; then
    common "new→$build" "$NEW" "$rt" "$pane"
  elif [ "$version" = "0.64.0" ]; then
    expect 0 "new→$build: session list" "$NEW" "$rt" session list
    expect 0 "new→$build: capture" "$NEW" "$rt" pane capture -s agent -p "$pane"
    printf '[session]\nname = "tpl"\n[[windows]]\ntitle = "w"\n[[windows.panes]]\ncommand = ["/bin/sh"]\n' >"$OUT/t.toml"
    expect 11 "new→$build: state apply is refused before it is sent" "$NEW" "$rt" state apply "$OUT/t.toml"
  else
    expect 10 "new→$build: refused, as a build before wire generations" "$NEW" "$rt" session list
    contains "new→$build: the advice names no overwritten CLI" "after its last session ends"
  fi
  new_runtime
  occupy "$NEW" "$rt" >/dev/null
  if [ -n "$wire" ]; then
    expect 0 "$build→new: session list" "$prev" "$rt" session list
  else
    # A shipped CLI before wire generations demands its own build: expected.
    expect 10 "$build→new: a CLI before wire generations still refuses" "$prev" "$rt" session list
  fi
done

if [ "$fails" -gt 0 ]; then
  printf '\033[1;31m✗ build skew: %d check(s) failed\033[0m\n' "$fails"
  exit 1
fi
printf '\033[1;32m✓ build skew: every pairing served\033[0m\n'
