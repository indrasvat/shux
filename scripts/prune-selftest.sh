#!/usr/bin/env bash
# Prove scripts/prune.sh keeps what is dirty, unpushed, in use, live, tracked,
# reached through a symlink or otherwise uncertain; removes the safe set;
# changes nothing on a dry run; re-checks each entry before removing it; never
# stops a daemon; and refuses paths outside their root. Then prove this suite
# goes red, for the right reason, when any one protection is taken out.
#
# Every case runs the real script inside throwaway repos, worktrees and temp
# roots under one mktemp dir; nothing outside it is a candidate.
set -euo pipefail

unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_OBJECT_DIRECTORY \
  GIT_ALTERNATE_OBJECT_DIRECTORIES GIT_COMMON_DIR GIT_NAMESPACE GIT_PREFIX GIT_QUARANTINE_PATH
while IFS= read -r var; do unset "${var}"; done < <(env | sed -n 's/^\(PRUNE_[A-Z_]*\)=.*/\1/p')
unset SHUX_NEUTRALITY_CACHE

SRC="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
work="$(cd "$(mktemp -d "${TMPDIR:-/tmp}/shux-prune-selftest.XXXXXX")" && pwd -P)"
: >"${work}/pids"
# A process this run started that is still listed: a zombie, or one whose args name
# this run's work dir (a reused pid names neither).
leftovers() {
  local pid line
  while read -r pid; do
    line="$(ps -o stat=,args= -p "${pid}" 2>/dev/null)" || continue
    case "${line}" in Z* | *"${work}"*) echo "${pid} ${line}" ;; esac
  done <"${work}/pids"
}

# Every fixture process, zombies included, must be gone: under an init that does
# not reap, as in a container, whatever a fixture leaves unreaped stays for good.
cleanup() {
  local pid i left=""
  while read -r pid; do kill "${pid}" 2>/dev/null || true; done <"${work}/pids"
  for i in $(seq 1 100); do
    left="$(leftovers)"
    [[ -n "${left}" ]] || break
    sleep 0.1
  done
  chmod -R u+rwx "${work}" 2>/dev/null || true
  find "${work}" -delete
  if [[ -n "${left}" ]]; then
    printf '✗ prune self-test: fixture processes outlived cleanup:\n%s\n' "${left}" >&2
    exit 1
  fi
}
trap cleanup EXIT

export GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL="${work}/gitconfig"
printf '[user]\n\tname = prune selftest\n\temail = selftest@invalid\n[init]\n\tdefaultBranch = main\n' >"${GIT_CONFIG_GLOBAL}"
printf '[commit]\n\tgpgsign = false\n[protocol "file"]\n\tallow = always\n[advice]\n\tdetachedHead = false\n' \
  >>"${GIT_CONFIG_GLOBAL}"

OLD=200001010000
RECENT="$(($(date +%Y) - 1))01010000"  # at most two years ago: inside DAYS
DAYS=800
AS_ROOT=0
[[ "$(id -u)" -ne 0 ]] || AS_ROOT=1  # mode bits stop nobody as root
backdate() { local stamp="$1"; shift; find "$@" -exec touch -t "${stamp}" {} +; }
# ago <path> <seconds>: everything under path modified exactly that long ago.
ago() {
  find "$1" -exec python3 -c 'import os, sys, time
t = time.time() - int(sys.argv[1])
for p in sys.argv[2:]: os.utime(p, (t, t))' "$2" {} +
}
blob() { mkdir -p "$(dirname "$1")"; head -c "${2:-65536}" /dev/zero >"$1"; }

# dead_pid: a pid the OS never assigns, so it names no process however long the
# run takes. A reaped child's pid does not do: on a busy machine the counter
# wraps and a live process gets it. Linux allocates below pid_max; XNU wraps
# before its PID_MAX, 99999.
if [[ -r /proc/sys/kernel/pid_max ]]; then
  NEVER_PID="$(cat /proc/sys/kernel/pid_max)"
else
  NEVER_PID=99999
fi
never_out="$(ps -p "${NEVER_PID}" -o pid= 2>&1)" && never_rc=0 || never_rc=$?
if [[ "${never_rc}" -ne 1 || -n "${never_out}" ]]; then
  echo "✗ prune self-test: ps does not report pid ${NEVER_PID} as no such process (exit ${never_rc}: ${never_out})" >&2
  exit 1
fi
dead_pid() { echo "${NEVER_PID}"; }

# The fakes this shell started. A subshell that starts fakes kills and reaps them
# on its way out (reap_mine): orphaned, they would each be a zombie under an init
# that does not reap.
MY_FAKES=()
reap_mine() {
  [[ "${#MY_FAKES[@]}" -gt 0 ]] || return 0
  kill "${MY_FAKES[@]}" 2>/dev/null || :
  wait "${MY_FAKES[@]}" 2>/dev/null || :
}

# start_fake <exe> <argv0> <cwd> <script>: perl (or a link to it, which names the
# process) running a script that sleeps until cleanup kills it; FAKE is its pid,
# returned once it has exec'd. Perl, not cat or sleep: coreutils built as one
# multicall binary (uutils, on ubuntu-latest) refuses to run under another name.
start_fake() {
  local exe="$1" argv0="$2" cwd="$3" arg="$4" script i args
  script="${arg}"
  [[ "${script}" == /* ]] || script="${cwd}/${arg}"
  [[ -e "${script}" ]] || echo 'sleep 3600 while 1;' >"${script}"
  # Its stdout detached, or a command substitution around the caller waits for it.
  (cd "${cwd}" && exec -a "${argv0}" "${exe}" "${arg}") </dev/null >/dev/null 2>"${script}.err" &
  FAKE=$!
  echo "${FAKE}" >>"${work}/pids"
  MY_FAKES+=("${FAKE}")
  for i in $(seq 1 300); do
    if [[ -r "/proc/${FAKE}/cmdline" ]]; then
      args="$(tr '\0' ' ' <"/proc/${FAKE}/cmdline" 2>/dev/null || true)"
    else
      args="$(ps -o args= -p "${FAKE}" 2>/dev/null || true)"
    fi
    [[ "${args}" != "${argv0} "* ]] || return 0
    sleep 0.1
  done
  echo "✗ prune self-test: ${argv0} never started; it shows as '${args}'" >&2
  ps -o pid=,stat=,comm=,args= -p "${FAKE}" >&2 || echo "  (pid ${FAKE} is gone)" >&2
  sed 's/^/  stderr: /' "${script}.err" >&2
  exit 1
}

# zombie_cargo <fixture>: a perl parent that forks a `cargo` child and leaves it a
# zombie until TERM, when it reaps it and exits; returned once the child is a zombie.
zombie_cargo() {
  local parent i z
  perl -e 'my $c = fork(); if (!$c) { exec($ARGV[0], "-e", "exit 0") }
    $SIG{TERM} = sub { waitpid($c, 0); exit 0 }; sleep 3600' "$1/bin/cargo" </dev/null >/dev/null 2>&1 &
  parent=$!
  echo "${parent}" >>"${work}/pids"
  MY_FAKES+=("${parent}")
  for i in $(seq 1 300); do
    z="$(ps -axo pid=,ppid=,stat= | awk -v p="${parent}" '$2 == p && $3 ~ /^Z/ { print $1 }')"
    if [[ -n "${z}" ]]; then
      echo "${z}" >>"${work}/pids"
      return 0
    fi
    sleep 0.1
  done
  echo "✗ prune self-test: the zombie cargo never appeared" >&2
  exit 1
}

# ── Fixture ────────────────────────────────────────────────────────────────
wt_add() { git -C "$1" worktree add -q -b "$2" "$3"; }
commit_push() { git -C "$1" add -f -- "${@:3}"; git -C "$1" commit -qm "$2"; git -C "$1" push -q origin HEAD; }

fixture() {
  local f="$1" script="$2" repo dead wt nl
  repo="${f}/repo"
  mkdir -p "${f}/bin"
  ln -s "$(command -v perl)" "${f}/bin/cargo"
  git init -q --bare "${f}/origin.git"
  git init -q "${repo}"
  mkdir -p "${repo}/scripts" "${repo}/.shux/scripts/lib" "${repo}/.shux/qa/kept" "${repo}/fuzz" "${repo}/crates"
  cp "${script}" "${repo}/scripts/prune.sh"
  cp "${SRC}/scripts/check-target-hygiene.sh" "${repo}/scripts/"
  cp "${SRC}/.shux/scripts/reap_daemons.sh" "${repo}/.shux/scripts/"
  cp "${SRC}/.shux/scripts/lib/proc_scope.sh" "${SRC}/.shux/scripts/lib/cache_lock.sh" "${repo}/.shux/scripts/lib/"
  printf '[workspace]\n' >"${repo}/Cargo.toml"
  printf '[workspace]\n' >"${repo}/fuzz/Cargo.toml"
  nl="${repo}/crates"$'\n'"sub"
  mkdir -p "${nl}"
  printf '[workspace]\n' >"${nl}/Cargo.toml"
  echo 'pub fn f() {}' >"${repo}/crates/lib.rs"
  printf 'target/\n.shux/out/\n.claude/worktrees/\n' >"${repo}/.gitignore"
  echo base >"${repo}/file.txt"
  echo evidence >"${repo}/.shux/qa/kept/SOLID-QA.md"
  blob "${repo}/.shux/out/tracked/keep.txt" 16
  # A configured cache and a runtime root inside the checkout, with committed contents.
  dead="$(dead_pid)"
  blob "${repo}/ncache/target/release/shux"
  rt "${repo}/rtroot/shuxtracked/shux/shux.pid" "${dead}"
  git -C "${repo}" add -- Cargo.toml fuzz crates "crates"$'\n'"sub" .gitignore file.txt scripts .shux/qa .shux/scripts
  git -C "${repo}" add -f -- .shux/out/tracked/keep.txt ncache rtroot
  git -C "${repo}" commit -qm base
  backdate "${OLD}" "${repo}/ncache" "${repo}/rtroot"
  git -C "${repo}" remote add origin "${f}/origin.git"
  git -C "${repo}" push -q origin main

  wt_add "${repo}" feat/pushed "${f}/wt-pushed"
  blob "${f}/wt-pushed/.shux/out/test-inventory/baseline.txt" 16
  commit_push "${f}/wt-pushed" pushed .shux/out/test-inventory/baseline.txt

  # Squash-merged: the branch's commit never reaches a remote, its change does.
  wt_add "${repo}" feat/squashed "${f}/wt-merged"
  echo squashed >"${f}/wt-merged/file.txt"
  git -C "${f}/wt-merged" commit -qam "squash me"
  echo squashed >"${repo}/file.txt"
  git -C "${repo}" commit -qam "squashed (#1)"
  git -C "${repo}" push -q origin main

  wt_add "${repo}" feat/dirty "${f}/wt-dirty"
  git -C "${f}/wt-dirty" push -q origin feat/dirty
  echo "unsaved work" >>"${f}/wt-dirty/file.txt"

  wt_add "${repo}" feat/unpushed "${f}/wt-unpushed"
  echo mine >"${f}/wt-unpushed/mine.txt"
  git -C "${f}/wt-unpushed" add -- mine.txt
  git -C "${f}/wt-unpushed" commit -qm unpushed

  # A tracked .shux/out that is a symlink into the worktree.
  wt_add "${repo}" feat/sym "${f}/wt-sym"
  blob "${f}/wt-sym/important/test-inventory/keep.txt" 16
  ln -s ../important "${f}/wt-sym/.shux/out"
  commit_push "${f}/wt-sym" sym important .shux/out

  # Listed by git at a path that is now a symlink to where it moved.
  wt_add "${repo}" feat/moved "${f}/wt-real"
  git -C "${f}/wt-real" push -q origin feat/moved
  mv "${f}/wt-real" "${f}/wt-moved"
  ln -s "${f}/wt-moved" "${f}/wt-real"

  wt_add "${repo}" feat/fuzz "${f}/wt-fuzz"
  git -C "${f}/wt-fuzz" push -q origin feat/fuzz
  wt_add "${repo}" feat/gone "${f}/wt-gone"
  git -C "${f}/wt-gone" push -q origin feat/gone

  git init -q "${f}/subrepo"
  echo s >"${f}/subrepo/s.txt"
  git -C "${f}/subrepo" add -- s.txt
  git -C "${f}/subrepo" commit -qm sub
  wt_add "${repo}" feat/sub "${f}/wt-sub"
  git -C "${f}/wt-sub" submodule add -q "${f}/subrepo" vendor/sub
  commit_push "${f}/wt-sub" sub .gitmodules vendor/sub
  wt_add "${repo}" feat/subdirty "${f}/wt-subdirty"
  git -C "${f}/wt-subdirty" submodule add -q "${f}/subrepo" vendor/sub
  git -C "${f}/wt-subdirty" config -f .gitmodules submodule.vendor/sub.ignore dirty
  commit_push "${f}/wt-subdirty" subdirty .gitmodules vendor/sub
  echo changed >"${f}/wt-subdirty/vendor/sub/s.txt"

  # A worktree path whose newline would read as a second `worktree` record.
  git clone -q "${f}/origin.git" "${f}/victim"
  wt_add "${repo}" feat/nl "${f}/wt"$'\n'"worktree ${f}/victim"

  wt_add "${f}/wt-pushed" feat/busy "${f}/wt-pushed/.claude/worktrees/busy"
  git -C "${f}/wt-pushed/.claude/worktrees/busy" push -q origin feat/busy

  for wt in "${repo}" "${f}/wt-pushed" "${f}/wt-merged" "${f}/wt-dirty" "${f}/wt-unpushed" "${f}/wt-moved" \
    "${f}/wt-fuzz" "${f}/wt-gone" "${f}/wt-sub" "${f}/wt-subdirty" "${f}/victim" \
    "${f}/wt-pushed/.claude/worktrees/busy"; do
    blob "${wt}/target/debug/deps/libx.rlib"
    blob "${wt}/.shux/out/stale/run.log"
    backdate "${OLD}" "${wt}/target" "${wt}/.shux/out/stale"
  done
  blob "${f}/wt-pushed/crates"$'\n'"sub/target/debug/x"
  blob "${f}/wt-pushed/.shux/out/..stale/x"
  backdate "${OLD}" "${f}/wt-pushed/crates"$'\n'"sub/target" "${f}/wt-pushed/.shux/out"
  backdate "${OLD}" "${f}/wt-sym/important"
  blob "${f}/wt-fuzz/fuzz/target/debug/fresh"
  blob "${repo}/.shux/out/fresh/run.log"
  backdate "${RECENT}" "${repo}/.shux/out/fresh"
  blob "${repo}/.shux/out/test-inventory/target/debug/x"
  backdate "${OLD}" "${repo}/.shux/out/test-inventory" "${repo}/.shux/out/tracked"

  # Running "cargo"s: in a worktree nested in wt-pushed, and in wt-sub's submodule.
  start_fake "${f}/bin/cargo" "${f}/bin/cargo" "${f}/wt-pushed/.claude/worktrees/busy" "${f}/busy.pl"
  start_fake "${f}/bin/cargo" "${f}/bin/cargo" "${f}/wt-sub/vendor/sub" "${f}/sub.pl"
  # A live shux daemon (a session) running repo's own build: args `<repo>/target/debug/shux __daemon`.
  mkdir -p "${f}/daemon"
  start_fake "$(command -v perl)" "${repo}/target/debug/shux" "${f}/daemon" __daemon
  DAEMON="${FAKE}"
  echo "${DAEMON}" >"${f}/daemon.pid"
  # Builds on the machine that are none of the repo's, as another suite's are: a cargo
  # whose scratch cwd was deleted, and a zombie cargo (exited, never reaped).
  mkdir -p "${f}/foreign"
  start_fake "${f}/bin/cargo" "${f}/bin/cargo" "${f}/foreign" "${f}/foreign.pl"
  find "${f}/foreign" -delete
  zombie_cargo "${f}"
  # The other half: a cargo whose deleted cwd was inside clean, pushed wt-gone still holds it.
  mkdir -p "${f}/wt-gone/scratch"
  start_fake "${f}/bin/cargo" "${f}/bin/cargo" "${f}/wt-gone/scratch" "${f}/gone.pl"
  find "${f}/wt-gone/scratch" -delete

  blob "${f}/neutral/bin/oldsha-tc/shux"
  blob "${f}/neutral/bin/newsha-tc/shux"
  blob "${f}/neutral/target/release/shux"
  blob "${f}/neutral-big/target/release/shux" $((2 * 1048576))
  blob "${f}/neutral-locked/target/release/shux"
  mkdir "${f}/neutral-locked/lock"
  blob "${f}/neutral-reading/target/release/shux"
  mkdir -p "${f}/neutral-reading/readers"
  echo "${DAEMON}" >"${f}/neutral-reading/readers/${DAEMON}"
  blob "${f}/neutral-real/target/release/shux"
  ln -s "${f}/neutral-real" "${f}/neutral-link"
  blob "${f}/neutral-binlink/src/precious/file.rs"
  ln -s src "${f}/neutral-binlink/bin"
  # A lock and reader marker left by a SIGKILLed run.
  blob "${f}/neutral-stale/target/release/shux"
  mkdir -p "${f}/neutral-stale/lock" "${f}/neutral-stale/readers"
  dead_pid >"${f}/neutral-stale/lock/pid"
  dead_pid >"${f}/neutral-stale/readers/1"
  backdate "${OLD}" "${f}"/neutral*
  backdate "${RECENT}" "${f}/neutral/bin/newsha-tc" "${f}/neutral-big/target"

  dead="$(dead_pid)"
  rt "${f}/tmp1/shuxlive/shux/shux.pid" "${DAEMON}"
  : >"${f}/tmp1/shuxlive/shux/shux.sock"
  rt "${f}/tmp1/shuxdead/shux/shux.pid" "${dead}"
  rt "${f}/tmp1/shuxdead/shux/shux.lock" "${dead}"
  : >"${f}/tmp1/shuxdead/shux/attach.sock"
  mkdir -p "${f}/tmp1/shuxdead/blesh"
  mkdir -p "${f}/tmp1/shuxnopid/shux"
  : >"${f}/tmp1/shuxnopid/shux/shux.start.lock"
  rt "${f}/tmp1/shuxother/shux/shux.pid" "${dead}"
  echo notes >"${f}/tmp1/shuxother/notes.txt"
  rt "${f}/tmp1/shuxdotdot/shux/shux.pid" "${dead}"
  rt "${f}/tmp1/shuxdotdot/..precious/data.txt" data
  rt "${f}/tmp1/shuxdotdot2/shux/shux.pid" "${dead}"
  rt "${f}/tmp1/shuxdotdot2/shux/..secret" data
  rt "${f}/tmp1/shuxclone/shux/Cargo.toml" "[package]"
  mkdir -p "${f}/tmp1/shuxmulti/shux"
  printf '%s\n%s\n' "${dead}" "${DAEMON}" >"${f}/tmp1/shuxmulti/shux/shux.pid"
  rt "${f}/tmp1/shuxspace/shux/shux.pid" "${dead:0:1} ${dead:1}"
  rt "${f}/outside/shuxtarget/shux/shux.pid" "${dead}"
  ln -s "${f}/outside/shuxtarget" "${f}/tmp1/shuxlink"
  if [[ "${AS_ROOT}" -eq 0 ]]; then
    rt "${f}/tmp1/shuxunread/shux/shux.pid" "$$"
    chmod 000 "${f}/tmp1/shuxunread/shux/shux.pid"
  fi
  rt "${f}/tmp2/shux-$(id -u)/shux.pid" "${dead}"
  rt "${f}/tmp 3 [x]*/shuxdead3/shux/shux.pid" "${dead}"
  backdate "${OLD}" "${f}/tmp1" "${f}/tmp2" "${f}/tmp 3 [x]*" "${f}/outside"
  rt "${f}/tmp1/shuxfresh/shux/shux.pid" "${dead}"
  ago "${f}/tmp1/shuxfresh" -86400  # recent however long the runs take
}

# ── Assertions ─────────────────────────────────────────────────────────────
OUT=""
fail() { printf '✗ prune self-test: %s\n%s\n' "$1" "${OUT}" >&2; exit 1; }
gone() { [[ ! -e "$1" && ! -L "$1" ]] || fail "$1 was kept, but is in the safe set"; }
kept() { [[ -e "$1" || -L "$1" ]] || fail "$1 was removed"; }

prune_env() {
  local f="$1"
  export PRUNE_TMP_ROOTS="${f}/tmp1:${f}/tmp2:${f}/tmp 3 [x]*:${f}/repo/rtroot" PRUNE_SCRATCH_DAYS="${DAYS}" \
    PRUNE_CACHE_DAYS="${DAYS}" PRUNE_CACHE_MIB="${CACHE_MIB:-5120}" SHUX_NEUTRALITY_CACHE="${NEUTRAL:-${f}/neutral}"
}

prune() {
  local f="$1" rc=0
  shift
  OUT="$(prune_env "${f}" && "${BASH}" "${f}/repo/scripts/prune.sh" "$@" 2>&1)" || rc=$?
  return "${rc}"
}

# safe_rm from the prune.sh under test, in a subshell: sourcing defines, never runs.
safe_rm_in() {
  local script="$1"
  shift
  # shellcheck source=scripts/prune.sh
  (. "${script}" && safe_rm "$@")
}

# neutral_apply <fixture>: plan and apply the vt-neutrality cache section alone.
neutral_apply() (
  prune_env "$1"
  # shellcheck source=scripts/prune.sh
  . "$1/repo/scripts/prune.sh"
  prune_setup
  plan_neutrality
  prune_apply
  prune_cleanup
)

# planned <path>: the plan holds exactly that path. A loop, not `printf | grep -q`:
# grep exits at its match, printf can still be writing, and pipefail turns its
# EPIPE into "not planned".
planned() {
  local p
  # shellcheck disable=SC2031  # called inside the same subshell that sourced prune.sh and planned
  for p in "${P_PATH[@]}"; do [[ "${p}" != "$1" ]] || return 0; done
  return 1
}

# late_apply <fixture>: plan, then start a build and a daemon, then apply that plan.
late_apply() (
  MY_FAKES=()
  trap reap_mine EXIT
  prune_env "$1"
  late="$1"
  # shellcheck source=scripts/prune.sh
  . "${late}/repo/scripts/prune.sh"
  prune_setup
  prune_plan >/dev/null
  planned "${late}/wt-pushed/target" || { echo "late target not planned"; exit 3; }
  planned "${late}/wt-merged/target" || { echo "late lock target not planned"; exit 3; }
  start_fake "${late}/bin/cargo" "${late}/bin/cargo" "${late}/wt-pushed" "${late}/late.pl"
  cat "${late}/daemon.pid" >"${late}/tmp1/shuxlate/shux/shux.pid"
  git -C "${late}/repo" worktree lock "${late}/wt-merged"
  prune_apply
  prune_cleanup
)

snapshot() { find "$1" \( -name .git -o -name origin.git -o -name modules \) -prune -o -print | LC_ALL=C sort; }

# suite <dir> <prune.sh to test> — exits non-zero at the first broken promise.
suite() {
  local f="$1" script="$2" repo rc out args nl
  MY_FAKES=()
  trap reap_mine EXIT  # suite always runs in a subshell of its own
  fixture "${f}" "${script}"
  repo="${f}/repo"
  nl="crates"$'\n'"sub"

  # Written "in the future": newer than prune's 30-minute reference however long the
  # runs take. The line itself is checked by busy_boundary, against that reference.
  ago "${f}/wt-fuzz/fuzz/target" -86400
  snapshot "${f}" >"${f}.before"
  rc=0
  prune "${f}" || rc=$?
  ps -p "$(cat "${f}/daemon.pid")" >/dev/null 2>&1 || fail "the live daemon (a shux session) was stopped"
  [[ "${rc}" -eq 0 ]] || fail "dry run exited ${rc}"
  snapshot "${f}" >"${f}.after"
  cmp -s "${f}.before" "${f}.after" || fail "dry run changed the tree: $(diff "${f}.before" "${f}.after" | sed -n '1,5p')"
  [[ "${OUT}" == *"make prune APPLY=1"* ]] || fail "dry run did not name the command to apply"

  prune "${f}" --apply || fail "apply exited non-zero"
  ps -p "$(cat "${f}/daemon.pid")" >/dev/null 2>&1 || fail "the live daemon (a shux session) was stopped"
  kept "${repo}/target"
  gone "${f}/wt-pushed/target"
  gone "${f}/wt-pushed/${nl}/target"
  kept "${f}/wt-pushed/crates/lib.rs"
  gone "${f}/wt-pushed/.shux/out/..stale"
  kept "${f}/wt-pushed/.shux/out/test-inventory/baseline.txt"
  gone "${f}/wt-merged/target"
  gone "${repo}/.shux/out/stale"
  gone "${repo}/.shux/out/test-inventory"
  gone "${f}/neutral/bin/oldsha-tc"
  gone "${f}/neutral/target"
  gone "${f}/tmp1/shuxdead"
  gone "${f}/tmp1/shuxnopid"
  gone "${f}/tmp2/shux-$(id -u)"
  gone "${f}/tmp 3 [x]*/shuxdead3"
  kept "${f}/wt-dirty/target"
  kept "${f}/wt-dirty/.shux/out/stale"
  grep -q "unsaved work" "${f}/wt-dirty/file.txt" || fail "the dirty worktree's edit was lost"
  kept "${f}/wt-unpushed/target"
  kept "${f}/wt-unpushed/mine.txt"
  kept "${f}/wt-pushed/.claude/worktrees/busy/target"
  kept "${f}/wt-sym/important/test-inventory/keep.txt"
  kept "${f}/wt-moved/target"
  kept "${f}/wt-fuzz/fuzz/target/debug/fresh"
  kept "${f}/wt-gone/target"
  kept "${f}/wt-sub/target"
  kept "${f}/wt-subdirty/target"
  kept "${f}/victim/target"
  kept "${repo}/.shux/out/fresh"
  kept "${repo}/.shux/out/tracked/keep.txt"
  kept "${repo}/.shux/qa/kept/SOLID-QA.md"
  kept "${repo}/file.txt"
  kept "${f}/neutral/bin/newsha-tc"
  kept "${f}/tmp1/shuxlive"
  kept "${f}/tmp1/shuxfresh"
  kept "${f}/tmp1/shuxother/notes.txt"
  kept "${f}/tmp1/shuxdotdot/..precious/data.txt"
  kept "${f}/tmp1/shuxdotdot2/shux/..secret"
  kept "${f}/tmp1/shuxclone/shux/Cargo.toml"
  kept "${f}/tmp1/shuxmulti"
  kept "${f}/tmp1/shuxspace"
  kept "${repo}/rtroot/shuxtracked/shux/shux.pid"
  kept "${f}/tmp1/shuxlink"
  kept "${f}/outside/shuxtarget/shux/shux.pid"
  [[ "${AS_ROOT}" -eq 1 ]] || kept "${f}/tmp1/shuxunread/shux"

  for n in big locked reading link binlink stale; do
    rc=0
    OUT="$(NEUTRAL="${f}/neutral-${n}" CACHE_MIB=1 neutral_apply "${f}" 2>&1)" || rc=$?
    [[ "${rc}" -eq 0 ]] || fail "neutral-${n} apply exited ${rc}"
  done
  rc=0
  OUT="$(NEUTRAL="${repo}/ncache" CACHE_MIB=1 neutral_apply "${f}" 2>&1)" || rc=$?
  [[ "${rc}" -eq 0 ]] || fail "in-checkout cache apply exited ${rc}"
  kept "${repo}/ncache/target/release/shux"
  gone "${f}/neutral-big/target"
  gone "${f}/neutral-stale/target"
  gone "${f}/neutral-stale/lock"
  kept "${f}/neutral-locked/target"
  kept "${f}/neutral-reading/target"
  kept "${f}/neutral-real/target"
  kept "${f}/neutral-binlink/src/precious/file.rs"

  # A plan gone stale: a build starts and a daemon comes alive after planning.
  # A plan gone stale: a build starts, a daemon comes alive and a worktree is locked after planning.
  blob "${f}/wt-pushed/target/debug/late"
  blob "${f}/wt-merged/target/debug/late"
  rt "${f}/tmp1/shuxlate/shux/shux.pid" "$(dead_pid)"
  backdate "${OLD}" "${f}/wt-pushed/target" "${f}/wt-merged/target" "${f}/tmp1/shuxlate"
  rc=0
  out="$(late_apply "${f}")" || rc=$?
  OUT="${out}"
  [[ "${rc}" -eq 0 ]] || fail "the stale-plan apply exited ${rc}"
  kept "${f}/wt-pushed/target"
  kept "${f}/tmp1/shuxlate"
  [[ -e "${f}/wt-merged/target" ]] || fail "late-locked wt-merged/target was removed"

  # ps that cannot answer: the pid might be running, so the runtime dir stays.
  rt "${f}/tmp1/shuxpsfail/shux/shux.pid" "$(dead_pid)"
  OUT="$(ps_fails_hold "${f}" "${f}/tmp1/shuxpsfail" 2>&1)" || :
  [[ "${OUT}" == *"ps cannot say"* ]] || fail "a ps failure was not held"

  # A lock left by a dead pid is replaced; one held by a live pid is not, even by a
  # caller that read the dead holder before another caller replaced it.
  rc=0
  OUT="$(lock_cases "${f}" "${repo}/.shux/scripts/lib/cache_lock.sh" 2>&1)" || rc=$?
  [[ "${rc}" -eq 0 ]] || fail "cache lock: ${OUT}"

  # The 30-minute line, to the second, on either side of prune's own reference file.
  OUT="$(busy_boundary "${f}" 2>&1)" || fail "busy boundary: ${OUT}"

  # A build whose cwd cannot be read (another user's, under sudo) holds every worktree.
  OUT="$(unreadable_cwd "${f}" 2>&1)" || :
  [[ -n "${OUT}" ]] || fail "an unreadable build cwd was ignored"

  # A build dir written to while an earlier entry of its worktree was being removed stays.
  wt_add "${repo}" feat/two "${f}/wt-two"
  git -C "${f}/wt-two" push -q origin feat/two
  blob "${f}/wt-two/target/debug/x"
  blob "${f}/wt-two/fuzz/target/debug/x"
  backdate "${OLD}" "${f}/wt-two/target" "${f}/wt-two/fuzz/target"
  rc=0
  OUT="$(mid_apply "${f}" 2>&1)" || rc=$?
  [[ "${rc}" -eq 0 ]] || fail "the mid-apply run exited ${rc}"
  gone "${f}/wt-two/target"
  [[ -e "${f}/wt-two/fuzz/target/debug/during" ]] || fail "mid-apply fuzz/target was removed"

  # safe_rm itself: empty, `/`, relative, outside the root, `..`, a symlink on the way, newlines.
  mkdir -p "${f}/root/inside" "${f}/root/real/victim" "${f}/elsewhere" "${f}/root/dir"$'\n'
  ln -s "${f}/elsewhere" "${f}/root/link"
  ln -s "${f}/root/real" "${f}/root/linkdir"
  ln -s "${f}/elsewhere" "${f}/root/dir"$'\n'"/victim"
  for args in "|${f}/root" "${f}/root/inside|" "/|${f}/root" "rel/x|${f}/root" "${f}/elsewhere|${f}/root" \
    "${f}/root/inside/../../elsewhere|${f}/root" "${f}/root/link|${f}/root" "${f}/root/..|${f}/root" \
    "${f}/root/linkdir/victim|${f}/root" "${f}/elsewhere/x|${f}/root" "${f}/root/inside|${f}/root"$'\n' \
    "${f}/root/dir"$'\n'"/victim|${f}/root"; do
    rc=0
    OUT="$(safe_rm_in "${script}" "${args%%|*}" "${args#*|}" 2>&1)" || rc=$?
    [[ "${rc}" -ne 0 ]] || fail "safe_rm '${args%%|*}' under '${args#*|}' was not refused"
  done
  kept "${f}/elsewhere"
  kept "${f}/root/inside"
  kept "${f}/root/real/victim"
  safe_rm_in "${script}" "${f}/root/inside" "${f}/root" || fail "safe_rm refused a path inside its root"
  gone "${f}/root/inside"

  rc=0
  OUT="$(PRUNE_TMP_ROOTS=relative "${BASH}" "${repo}/scripts/prune.sh" 2>&1)" || rc=$?
  [[ "${rc}" -eq 2 && "${OUT}" == *"not absolute"* ]] || fail "a relative runtime root was not refused (exit ${rc})"

  # A worktree whose build dirs cannot be listed fails the run.
  wt_add "${repo}" feat/bad "${f}/wt-bad"
  printf '[workspace\n' >"${f}/wt-bad/Cargo.toml"
  commit_push "${f}/wt-bad" bad Cargo.toml
  rc=0
  prune "${f}" || rc=$?
  [[ "${rc}" -ne 0 && "${OUT}" == *"could not list"* ]] || fail "a bad manifest did not fail the run (exit ${rc})"
}

# ps_fails_hold <fixture> <runtime dir>: runtime_hold's verdict when ps errors.
ps_fails_hold() (
  prune_env "$1"
  # shellcheck source=scripts/prune.sh
  . "$1/repo/scripts/prune.sh"
  prune_setup
  # shellcheck disable=SC2329  # shadows ps for pid_state, which calls it by name
  ps() { echo "ps: cannot read the process table" >&2; return 1; }
  runtime_hold "$2" "$2/shux"
  prune_cleanup
)

# lock_cases <fixture> <cache_lock.sh>: cache_lock_take replaces a dead holder's
# lock, never a live one's, and never two callers' at once.
lock_cases() (
  # shellcheck source=.shux/scripts/lib/cache_lock.sh
  . "$2"
  live="$(cat "$1/daemon.pid")"
  rm -rf "$1/locks"
  mkdir -p "$1/locks/stale" "$1/locks/live" "$1/locks/race"
  dead_pid >"$1/locks/stale/pid"
  echo "${live}" >"$1/locks/live/pid"
  cache_lock_take "$1/locks/stale" 1 || { echo "a dead holder's lock was not replaced"; exit 1; }
  [[ "$(cat "$1/locks/stale/pid")" == "$$" ]] || { echo "the replaced lock does not name its new holder"; exit 1; }
  cache_lock_release "$1/locks/stale" || { echo "release failed"; exit 1; }
  if cache_lock_take "$1/locks/live" 1; then echo "a live holder's lock was taken"; exit 1; fi
  # Another caller breaks the dead holder's lock and takes it between our read and our claim.
  race="$1/locks/race" dead="$(dead_pid)"
  echo "${dead}" >"${race}/pid"
  eval "real_$(declare -f pid_state)"
  # shellcheck disable=SC2329  # shadows pid_state for cache_lock_take, which calls it by name
  pid_state() {
    if [[ "$1" == "${dead}" && ! -e "${race}.swapped" ]]; then
      : >"${race}.swapped"
      rm -f "${race}/pid"
      rmdir "${race}"
      mkdir "${race}"
      echo "${live}" >"${race}/pid"
      echo dead
      return
    fi
    real_pid_state "$1"
  }
  if cache_lock_take "${race}" 1; then echo "two callers both hold the lock after a stale-lock race"; exit 1; fi
  [[ "$(cat "${race}/pid" 2>/dev/null)" == "${live}" ]] || { echo "a stale-lock race moved the live lock away"; exit 1; }
)

# busy_boundary <fixture>: fresh_build against prune's reference, 5 s either side of it.
busy_boundary() (
  prune_env "$1"
  # shellcheck source=scripts/prune.sh
  . "$1/repo/scripts/prune.sh"
  prune_setup
  mkdir -p "$1/edge"
  : >"$1/edge/inside"
  : >"$1/edge/outside"
  python3 -c 'import os, sys
ref = os.stat(sys.argv[1]).st_mtime
os.utime(sys.argv[2], (ref + 5, ref + 5))
os.utime(sys.argv[3], (ref - 5, ref - 5))' "${BUSY_REF}" "$1/edge/inside" "$1/edge/outside"
  [[ -n "$(fresh_build "$1/edge/inside")" ]] || { echo "a build dir 5 s inside the line was not in use"; exit 1; }
  [[ -z "$(fresh_build "$1/edge/outside")" ]] || { echo "a build dir 5 s outside the line was in use"; exit 1; }
  prune_cleanup
)

# unreadable_cwd <fixture>: CWD_UNKNOWN after scan_builds when no build's cwd can be read.
unreadable_cwd() (
  prune_env "$1"
  # shellcheck source=scripts/prune.sh
  . "$1/repo/scripts/prune.sh"
  prune_setup
  # shellcheck disable=SC2329  # shadows pid_cwd for scan_builds, which calls it by name
  pid_cwd() { :; }
  scan_builds
  printf '%s' "${CWD_UNKNOWN}"
  prune_cleanup
)

# mid_apply <fixture>: plan, then apply with a write to wt-two/fuzz/target landing
# just after wt-two/target is removed.
mid_apply() (
  prune_env "$1"
  mid="$1"
  # shellcheck source=scripts/prune.sh
  . "${mid}/repo/scripts/prune.sh"
  prune_setup
  plan_worktrees >/dev/null
  planned "${mid}/wt-two/fuzz/target" || { echo "wt-two/fuzz/target not planned"; exit 3; }
  eval "real_$(declare -f safe_rm)"
  # shellcheck disable=SC2329  # shadows safe_rm for prune_apply, which calls it by name
  safe_rm() {
    real_safe_rm "$@" || return
    [[ "$1" != "${mid}/wt-two/target" ]] || blob "${mid}/wt-two/fuzz/target/debug/during"
  }
  prune_apply
  prune_cleanup
)
rt() { mkdir -p "$(dirname "$1")"; printf '%s\n' "$2" >"$1"; }

# ── Run: the real script passes; each mutant fails for its own reason ──────
PRUNE="${SRC}/scripts/prune.sh"
if ! (suite "${work}/real" "${PRUNE}"); then
  echo "✗ prune self-test: scripts/prune.sh broke a promise (above)" >&2
  exit 1
fi
[[ "${AS_ROOT}" -eq 0 ]] || echo "note: running as root, the unreadable-pidfile case was skipped" >&2
echo "✓ prune self-test: keeps dirty/unpushed/in-use/live/tracked/symlinked, removes the safe set, dry run inert"

M_NAME=() M_FROM=() M_TO=() M_WANT=()
# mutant <name> <text in prune.sh> <replacement> <failure the suite must report, or one per line>
mutant() { M_NAME+=("$1") M_FROM+=("$2") M_TO+=("$3") M_WANT+=("$4"); }
# shellcheck disable=SC2016  # prune.sh source text, never expanded here
{
  mutant dirty 'if [[ -n "${st}" ]]; then' 'if false; then' 'wt-dirty/target was removed'
  mutant unpushed 'if [[ "${n}" -gt 0 ]]; then' 'if false; then' 'wt-unpushed/target was removed'
  mutant busy 'if [[ "$(owner_of "${B_CWD[${i}]}")" == "$1" ]]; then' 'if false; then' 'busy/target was removed'
  mutant 'fresh build dir' 'if touched_within "$1" "${BUSY_REF}"; then echo "$1 written' \
    'if false; then echo "$1 written' 'fresh was removed'
  mutant 'live daemon pid' 'alive) echo "pid ${PIDFILE_PID}' 'alive) : "pid ${PIDFILE_PID}' 'shuxlive was removed'
  mutant 'ps failure' 'unknown) echo "ps cannot say' 'unknown) : "ps cannot say' 'a ps failure was not held'
  mutant 'lock at apply' 'WT_HOLD="$(worktree_hold "${ctx}" "$(wt_flag "${ctx}")")"' \
    'WT_HOLD="$(worktree_hold "${ctx}" "")"' 'late-locked wt-merged/target was removed'
  mutant 'tracked in a checkout' '  tracked_hold "${top}" "$1"' '  :' \
    $'shuxtracked/shux/shux.pid was removed\nncache/target/release/shux was removed'
  mutant 'tracked runtime' '  h="$(checkout_hold "${e}")"' '  h=""' 'shuxtracked/shux/shux.pid was removed'
  mutant 'list failure' '[[ "${hold}" != "${LIST_FAILED}" ]] || HYGIENE_FAILED' \
    'true || HYGIENE_FAILED' 'a bad manifest did not fail the run'
  mutant 'stale lock' '[[ "$(pid_state "${PIDFILE_PID}")" != dead ]]; then' 'true; then' 'neutral-stale/target was kept'
  mutant 'daemon build' '[[ -z "${pids// /}" ]] || echo "a live shux daemon' 'true || echo "a live shux daemon' \
    'repo/target was removed'
  mutant 'reaps daemons' '"${ROOT}/.shux/scripts/reap_daemons.sh" --dry-run 2>&1' \
    '"${ROOT}/.shux/scripts/reap_daemons.sh" 2>&1' \
    $'the live daemon (a shux session) was stopped\nreap_daemons.sh --dry-run failed'
  mutant tracked 'elif [[ -n "${out}" ]]; then' 'elif false; then' 'baseline.txt was removed'
  mutant 'dry run applies' 'if [[ "${apply}" -eq 1 ]]; then' 'if true; then' 'dry run changed the tree'
  mutant 'root containment' 'if ! below "${root}" "${path}"; then' 'if false; then' 'was not refused'
  mutant 'neutrality reader' 'if [[ -n "${h}" ]]; then echo "reader marker: ${h}"' \
    'if false; then echo "reader marker: ${h}"' 'neutral-reading/target was removed'
  mutant 'stale plan' '    recheck "${i}"' '    HOLD=""' $'wt-pushed/target was removed\nneutral-stale/lock was kept'
  mutant 'squash merge' '== "${BASE_TREE}" ]]; }; then' '== "never" ]]; }; then' 'wt-merged/target was kept'
  mutant inert '      plan_worktree "${WTS[${i}]}"' '      :' 'wt-pushed/target was kept'
  mutant 'per-item build recheck' 'if [[ -z "${HOLD}" && "${kind}" == build ]]; then HOLD="$(build_activity_hold' \
    'if false; then HOLD="$(build_activity_hold' 'mid-apply fuzz/target was removed'
  mutant 'unreadable cwd' '[[ "$(pid_state "${pid}")" == dead ]] || CWD_UNKNOWN="cannot see the cwd' \
    'true || CWD_UNKNOWN="cannot see the cwd' 'an unreadable build cwd was ignored'
  mutant 'foreign deleted cwd' '    B_PID+=("${pid}") B_NAME+=("${name}") B_CWD+=("${cwd}")' \
    '    [[ -d "${cwd}" ]] || { CWD_UNKNOWN=gone; continue; }; B_PID+=("${pid}") B_NAME+=("${name}") B_CWD+=("${cwd}")' \
    'wt-pushed/target was kept'
  mutant 'deleted cwd skipped' '    B_PID+=("${pid}") B_NAME+=("${name}") B_CWD+=("${cwd}")' \
    '    [[ -d "${cwd}" ]] || continue; B_PID+=("${pid}") B_NAME+=("${name}") B_CWD+=("${cwd}")' \
    'wt-gone/target was removed'
}

# mutate <file> <from> <to> <out>: the first occurrence replaced, literally
# (ENVIRON: no escape processing; Bash 3.2's ${var/…/…} takes tens of seconds here).
mutate() {
  F="$2" T="$3" awk 'BEGIN { f = ENVIRON["F"]; t = ENVIRON["T"] }
    !done && (p = index($0, f)) { $0 = substr($0, 1, p - 1) t substr($0, p + length(f)); done = 1 }
    { print }' "$1" >"$4"
  if cmp -s "$1" "$4"; then
    echo "✗ prune self-test: a mutant no longer applies to $1: $2" >&2
    exit 1
  fi
}

# The lock library's claim check, taken out: the race case must catch it.
# shellcheck disable=SC2016  # cache_lock.sh source text
mutate "${SRC}/.shux/scripts/lib/cache_lock.sh" \
  'if pidfile_pid "${claimed}/pid" && [[ "${PIDFILE_PID}" == "${dead}" ]]; then' 'if true; then' \
  "${work}/cache_lock.mutant.sh"
mkdir -p "${work}/lockmut"
echo "$$" >"${work}/lockmut/daemon.pid"
if (lock_cases "${work}/lockmut" "${work}/cache_lock.mutant.sh") >"${work}/lockmut.log" 2>&1; then
  echo "✗ prune self-test: passed with the stale-lock claim check removed from cache_lock.sh" >&2
  exit 1
elif ! grep -q "stale-lock race" "${work}/lockmut.log"; then
  echo "✗ prune self-test: the cache_lock.sh mutant failed for the wrong reason: $(cat "${work}/lockmut.log")" >&2
  exit 1
fi

# Each mutant in its own fixture, all at once; then each verdict is read.
jobs_=()
for i in "${!M_NAME[@]}"; do
  mutate "${PRUNE}" "${M_FROM[${i}]}" "${M_TO[${i}]}" "${work}/mutant-${i}.sh"
  (suite "${work}/m${i}" "${work}/mutant-${i}.sh" >"${work}/m${i}.log" 2>&1) &
  jobs_+=("$!")
done
bad=0
for i in "${!M_NAME[@]}"; do
  rc=0
  wait "${jobs_[${i}]}" || rc=$?
  if [[ "${rc}" -eq 0 ]]; then
    echo "✗ prune self-test: passed with '${M_NAME[${i}]}' removed from prune.sh" >&2
    bad=1
  elif ! grep -qF -- "${M_WANT[${i}]}" "${work}/m${i}.log"; then
    printf '✗ prune self-test: mutant %s failed for the wrong reason (wanted "%s"):\n%s\n' "${M_NAME[${i}]}" \
      "${M_WANT[${i}]}" "$(head -3 "${work}/m${i}.log")" >&2
    bad=1
  fi
done
[[ "${bad}" -eq 0 ]] || exit 1
echo "✓ prune self-test: all ${#M_NAME[@]} mutants caught, each for its own reason"
