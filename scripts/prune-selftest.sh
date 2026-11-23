#!/usr/bin/env bash
# Prove scripts/prune.sh never removes a dirty, unpushed or in-use worktree's
# files, a live daemon's runtime dir, tracked scratch or .shux/qa; does remove
# the safe set; changes nothing on a dry run; and refuses empty and out-of-root
# paths. Then prove this suite goes red, for the right reason, when any one of
# those protections is taken out of prune.sh.
#
# Every case runs the real script inside throwaway git repos, worktrees and
# temp roots under one mktemp dir; nothing outside it is read for removal.
set -euo pipefail

unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_OBJECT_DIRECTORY \
  GIT_ALTERNATE_OBJECT_DIRECTORIES GIT_COMMON_DIR GIT_NAMESPACE GIT_PREFIX GIT_QUARANTINE_PATH
while IFS= read -r var; do unset "${var}"; done < <(env | sed -n 's/^\(PRUNE_[A-Z_]*\)=.*/\1/p')
unset SHUX_NEUTRALITY_CACHE

SRC="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
work="$(cd "$(mktemp -d "${TMPDIR:-/tmp}/shux-prune-selftest.XXXXXX")" && pwd -P)"
: >"${work}/pids"
cleanup() {
  local pid
  while read -r pid; do kill "${pid}" 2>/dev/null || true; done <"${work}/pids"
  chmod -R u+rwx "${work}" 2>/dev/null || true
  find "${work}" -delete
}
trap cleanup EXIT

export GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL="${work}/gitconfig"
printf '[user]\n\tname = prune selftest\n\temail = selftest@invalid\n[init]\n\tdefaultBranch = main\n[commit]\n\tgpgsign = false\n' \
  >"${GIT_CONFIG_GLOBAL}"

OLD=200001010000
RECENT="$(($(date +%Y) - 1))01010000"  # at most two years ago: inside DAYS
DAYS=800
backdate() { local stamp="$1"; shift; find "$@" -exec touch -t "${stamp}" {} +; }
blob() { mkdir -p "$(dirname "$1")"; head -c "${2:-65536}" /dev/zero >"$1"; }

dead_pid() {
  local pid
  while :; do
    (exit 0) &
    pid=$!
    wait "${pid}"
    ps -p "${pid}" >/dev/null 2>&1 || { echo "${pid}"; return; }
  done
}

# ── Fixture ────────────────────────────────────────────────────────────────
commit() { git -C "$1" commit -qm "$2"; }

fixture() {
  local f="$1" script="$2" repo dead wt bp comm i
  repo="${f}/repo"
  mkdir -p "${f}"
  git init -q --bare "${f}/origin.git"
  git init -q "${repo}"
  mkdir -p "${repo}/scripts" "${repo}/.shux/scripts/lib" "${repo}/.shux/qa/kept"
  cp "${script}" "${repo}/scripts/prune.sh"
  cp "${SRC}/scripts/check-target-hygiene.sh" "${repo}/scripts/"
  cp "${SRC}/.shux/scripts/reap_daemons.sh" "${repo}/.shux/scripts/"
  cp "${SRC}/.shux/scripts/lib/proc_scope.sh" "${repo}/.shux/scripts/lib/"
  printf '[workspace]\n' >"${repo}/Cargo.toml"
  printf 'target/\n.shux/out/\n.claude/worktrees/\n' >"${repo}/.gitignore"
  echo base >"${repo}/file.txt"
  echo evidence >"${repo}/.shux/qa/kept/SOLID-QA.md"
  blob "${repo}/.shux/out/tracked/keep.txt" 16
  git -C "${repo}" add -- Cargo.toml .gitignore file.txt scripts .shux/qa .shux/scripts
  git -C "${repo}" add -f -- .shux/out/tracked/keep.txt
  commit "${repo}" base
  git -C "${repo}" remote add origin "${f}/origin.git"
  git -C "${repo}" push -q origin main

  git -C "${repo}" worktree add -q -b feat/pushed "${f}/wt-pushed"
  echo pushed >"${f}/wt-pushed/pushed.txt"
  git -C "${f}/wt-pushed" add -- pushed.txt
  commit "${f}/wt-pushed" pushed
  git -C "${f}/wt-pushed" push -q origin feat/pushed

  # Squash-merged: the branch's commit never reaches a remote, its change does.
  git -C "${repo}" worktree add -q -b feat/squashed "${f}/wt-merged"
  echo squashed >"${f}/wt-merged/file.txt"
  git -C "${f}/wt-merged" commit -qam "squash me"
  echo squashed >"${repo}/file.txt"
  git -C "${repo}" commit -qam "squashed (#1)"
  git -C "${repo}" push -q origin main

  git -C "${repo}" worktree add -q -b feat/dirty "${f}/wt-dirty"
  git -C "${f}/wt-dirty" push -q origin feat/dirty
  echo "unsaved work" >>"${f}/wt-dirty/file.txt"

  git -C "${repo}" worktree add -q -b feat/unpushed "${f}/wt-unpushed"
  echo mine >"${f}/wt-unpushed/mine.txt"
  git -C "${f}/wt-unpushed" add -- mine.txt
  commit "${f}/wt-unpushed" unpushed

  # Nested in the main checkout: a build here must not hold the main one.
  git -C "${repo}" worktree add -q -b feat/busy "${repo}/.claude/worktrees/busy"
  git -C "${repo}/.claude/worktrees/busy" push -q origin feat/busy

  for wt in "${repo}" "${f}/wt-pushed" "${f}/wt-merged" "${f}/wt-dirty" "${f}/wt-unpushed" \
    "${repo}/.claude/worktrees/busy"; do
    blob "${wt}/target/debug/deps/libx.rlib"
    blob "${wt}/.shux/out/stale/run.log"
    backdate "${OLD}" "${wt}/target" "${wt}/.shux/out/stale"
  done
  blob "${repo}/.shux/out/fresh/run.log"
  backdate "${RECENT}" "${repo}/.shux/out/fresh"
  blob "${repo}/.shux/out/test-inventory/target/debug/x"
  backdate "${OLD}" "${repo}/.shux/out/test-inventory" "${repo}/.shux/out/tracked"

  # A running "cargo" whose cwd is the busy worktree: cat, named cargo, blocked on a fifo.
  mkdir -p "${f}/bin"
  ln -s "$(command -v cat)" "${f}/bin/cargo"
  mkfifo "${f}/fifo"
  (cd "${repo}/.claude/worktrees/busy" && exec "${f}/bin/cargo" <"${f}/fifo") &
  bp=$!
  echo "${bp}" >>"${work}/pids"
  exec 8>"${f}/fifo"
  for i in $(seq 1 100); do
    comm="$(ps -o comm= -p "${bp}" 2>/dev/null || true)"
    [[ "${comm##*/}" != cargo ]] || break
    [[ "${i}" -lt 100 ]] || { echo "✗ prune self-test: fake cargo never started" >&2; exit 1; }
    sleep 0.1
  done

  for n in big locked; do
    blob "${f}/neutral-${n}/target/release/shux" $((2 * 1048576))
  done
  mkdir "${f}/neutral-locked/lock"
  blob "${f}/neutral/bin/oldsha-tc/shux"
  blob "${f}/neutral/bin/newsha-tc/shux"
  blob "${f}/neutral/target/release/shux"
  backdate "${OLD}" "${f}/neutral" "${f}/neutral-big" "${f}/neutral-locked"
  backdate "${RECENT}" "${f}/neutral/bin/newsha-tc" "${f}/neutral-big/target"

  dead="$(dead_pid)"
  rt() { mkdir -p "$(dirname "$1")"; printf '%s\n' "$2" >"$1"; }
  rt "${f}/tmp1/shuxlive/shux/shux.pid" "$$"
  : >"${f}/tmp1/shuxlive/shux/shux.sock"
  rt "${f}/tmp1/shuxdead/shux/shux.pid" "${dead}"
  rt "${f}/tmp1/shuxdead/shux/shux.lock" "${dead}"
  : >"${f}/tmp1/shuxdead/shux/attach.sock"
  mkdir -p "${f}/tmp1/shuxdead/blesh"
  mkdir -p "${f}/tmp1/shuxnopid/shux"
  : >"${f}/tmp1/shuxnopid/shux/shux.start.lock"
  rt "${f}/tmp1/shuxother/shux/shux.pid" "${dead}"
  echo notes >"${f}/tmp1/shuxother/notes.txt"
  rt "${f}/tmp1/shuxclone/shux/Cargo.toml" "[package]"
  rt "${f}/outside/shuxtarget/shux/shux.pid" "${dead}"
  ln -s "${f}/outside/shuxtarget" "${f}/tmp1/shuxlink"
  rt "${f}/tmp2/shux-$(id -u)/shux.pid" "${dead}"
  backdate "${OLD}" "${f}/tmp1" "${f}/tmp2" "${f}/outside"
  rt "${f}/tmp1/shuxfresh/shux/shux.pid" "${dead}"
}

# ── Assertions ─────────────────────────────────────────────────────────────
OUT=""
fail() { printf '✗ prune self-test: %s\n%s\n' "$1" "${OUT}" >&2; exit 1; }
gone() { [[ ! -e "$1" && ! -L "$1" ]] || fail "$1 was kept, but is in the safe set"; }
kept() { [[ -e "$1" || -L "$1" ]] || fail "$1 was removed"; }

prune() {
  local f="$1" rc=0
  shift
  OUT="$(env PRUNE_TMP_ROOTS="${f}/tmp1 ${f}/tmp2" PRUNE_SCRATCH_DAYS="${DAYS}" \
    PRUNE_CACHE_DAYS="${DAYS}" PRUNE_CACHE_MIB="${CACHE_MIB:-5120}" \
    SHUX_NEUTRALITY_CACHE="${NEUTRAL:-${f}/neutral}" \
    "${BASH}" "${f}/repo/scripts/prune.sh" "$@" 2>&1)" || rc=$?
  return "${rc}"
}

# safe_rm from the prune.sh under test, in a subshell: sourcing defines, never runs.
safe_rm_in() {
  local script="$1"
  shift
  # shellcheck source=scripts/prune.sh
  (. "${script}" && safe_rm "$@")
}

snapshot() { find "$1" \( -name .git -o -name origin.git \) -prune -o -print | LC_ALL=C sort; }

# suite <dir> <prune.sh to test> — exits non-zero at the first broken promise.
suite() {
  local f="$1" script="$2" repo rc out
  fixture "${f}" "${script}"
  repo="${f}/repo"

  snapshot "${f}" >"${f}.before"
  prune "${f}" || fail "dry run exited non-zero"
  snapshot "${f}" >"${f}.after"
  cmp -s "${f}.before" "${f}.after" || fail "dry run changed the tree: $(diff "${f}.before" "${f}.after" | head -5)"
  [[ "${OUT}" == *"make prune APPLY=1"* ]] || fail "dry run did not name the command to apply"

  prune "${f}" --apply || fail "apply exited non-zero"
  gone "${repo}/target"
  gone "${f}/wt-pushed/target"
  gone "${f}/wt-merged/target"
  gone "${repo}/.shux/out/stale"
  gone "${repo}/.shux/out/test-inventory"
  gone "${f}/neutral/bin/oldsha-tc"
  gone "${f}/neutral/target"
  gone "${f}/tmp1/shuxdead"
  gone "${f}/tmp1/shuxnopid"
  gone "${f}/tmp2/shux-$(id -u)"
  kept "${f}/wt-dirty/target"
  kept "${f}/wt-dirty/.shux/out/stale"
  grep -q "unsaved work" "${f}/wt-dirty/file.txt" || fail "the dirty worktree's edit was lost"
  kept "${f}/wt-unpushed/target"
  kept "${f}/wt-unpushed/mine.txt"
  kept "${repo}/.claude/worktrees/busy/target"
  kept "${repo}/.shux/out/fresh"
  kept "${repo}/.shux/out/tracked/keep.txt"
  kept "${repo}/.shux/qa/kept/SOLID-QA.md"
  kept "${repo}/file.txt"
  kept "${f}/neutral/bin/newsha-tc"
  kept "${f}/tmp1/shuxlive"
  kept "${f}/tmp1/shuxfresh"
  kept "${f}/tmp1/shuxother/notes.txt"
  kept "${f}/tmp1/shuxclone/shux/Cargo.toml"
  kept "${f}/tmp1/shuxlink"
  kept "${f}/outside/shuxtarget/shux/shux.pid"

  NEUTRAL="${f}/neutral-big" CACHE_MIB=1 prune "${f}" --apply || fail "size-limit apply exited non-zero"
  gone "${f}/neutral-big/target"
  NEUTRAL="${f}/neutral-locked" CACHE_MIB=1 prune "${f}" --apply || fail "locked apply exited non-zero"
  kept "${f}/neutral-locked/target"

  # safe_rm itself: empty input, `/`, outside the root, a symlink, `..`.
  mkdir -p "${f}/root/inside" "${f}/elsewhere"
  ln -s "${f}/elsewhere" "${f}/root/link"
  for args in "|${f}/root" "${f}/root/inside|" "/|${f}/root" "${f}/elsewhere|${f}/root" \
    "${f}/root/inside/../../elsewhere|${f}/root" "${f}/root/link|${f}/root" "${f}/root/..|${f}/root"; do
    rc=0
    out="$(safe_rm_in "${script}" "${args%%|*}" "${args#*|}" 2>&1)" || rc=$?
    OUT="${out}"
    [[ "${rc}" -ne 0 ]] || fail "safe_rm '${args%%|*}' under '${args#*|}' was not refused"
  done
  rc=0
  OUT="$(safe_rm_in "${script}" "" "${f}/root" 2>&1)" || rc=$?
  [[ "${rc}" -ne 0 ]] || fail "safe_rm '' succeeded"
  [[ "${OUT}" == *"empty path"* ]] || fail "safe_rm '' was not refused as an empty path"
  kept "${f}/elsewhere"
  kept "${f}/root/inside"
  safe_rm_in "${script}" "${f}/root/inside" "${f}/root" || fail "safe_rm refused a path inside its root"
  gone "${f}/root/inside"
}

# ── Run: the real script passes; each mutant fails for its own reason ──────
PRUNE="${SRC}/scripts/prune.sh"
if ! (suite "${work}/real" "${PRUNE}"); then
  echo "✗ prune self-test: scripts/prune.sh broke a promise (above)" >&2
  exit 1
fi
echo "✓ prune self-test: keeps dirty/unpushed/in-use/live/tracked, removes the safe set, dry run inert"

M_NAME=() M_FROM=() M_TO=() M_WANT=()
# mutant <name> <text in prune.sh> <replacement> <failure the suite must report>
mutant() { M_NAME+=("$1") M_FROM+=("$2") M_TO+=("$3") M_WANT+=("$4"); }
# shellcheck disable=SC2016  # prune.sh source text, never expanded here
{
  mutant dirty 'if [[ -n "${st}" ]]; then' 'if false; then' 'wt-dirty/target was removed'
  mutant unpushed 'if [[ "${n}" -gt 0 ]]; then' 'if false; then' 'wt-unpushed/target was removed'
  mutant busy 'if in_tree "${rest#* }" "${wt}"; then' 'if false; then' 'busy/target was removed'
  mutant 'live daemon' 'if ps -p "${pid}" >/dev/null 2>&1; then' 'if false; then' 'shuxlive was removed'
  mutant 'tracked scratch' 'ls-files -- ".shux/out/${name}"' 'ls-files -- ".shux/out/${name}.none"' 'keep.txt was removed'
  mutant 'dry run applies' 'if [[ "${apply}" -eq 0 ]]; then' 'if false; then' 'dry run changed the tree'
  mutant 'root containment' '"${real_root}"/?*) ;;' '*) ;;' 'was not refused'
  mutant 'empty guard' 'if [[ -z "${path}" || -z "${root}" ]]; then' 'if false; then' 'refused as an empty path'
  mutant 'squash merge' '== "${BASE_TREE}" ]]; then' '== "never" ]]; then' 'wt-merged/target was kept'
  mutant inert '      plan_worktree "${wt}"' '      :' 'repo/target was kept'
}

# Each mutant in its own fixture, all at once; then each verdict is read.
original="$(cat "${PRUNE}")"
jobs_=()
for i in "${!M_NAME[@]}"; do
  # The replacement unquoted: bash 3.2 keeps quotes there literally; none holds `&` or `\`.
  text="${original/"${M_FROM[${i}]}"/${M_TO[${i}]}}"
  if [[ "${text}" == "${original}" ]]; then
    echo "✗ prune self-test: mutant '${M_NAME[${i}]}' no longer applies to prune.sh" >&2
    exit 1
  fi
  printf '%s\n' "${text}" >"${work}/mutant-${i}.sh"
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
