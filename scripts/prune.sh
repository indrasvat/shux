#!/usr/bin/env bash
# Report, and with --apply remove, disk space shux development can regenerate.
# What it removes and what it never touches: docs/agents/disk.md.
#
#   scripts/prune.sh            dry run: the report, nothing changes
#   scripts/prune.sh --apply    the same plan; each entry is re-checked, then removed
#
# Paths are carried literally: NUL-delimited lists, no word splitting, trailing
# newlines kept. Anything uncertain (a symlink on the way, git or a pidfile that
# cannot answer) is kept. It never stops a daemon. Sourcing this file defines
# the functions without running anything.
set -euo pipefail

BUSY_MIN=30
BASE_REF=origin/main

prune_settings() {
  CACHE_MIB="${PRUNE_CACHE_MIB:-5120}"
  CACHE_DAYS="${PRUNE_CACHE_DAYS:-14}"
  SCRATCH_DAYS="${PRUNE_SCRATCH_DAYS:-7}"
  NEUTRALITY="${SHUX_NEUTRALITY_CACHE:-${XDG_CACHE_HOME:-${HOME:-}/.cache}/shux/vt-neutrality}"
  local name value spec r
  for name in CACHE_MIB CACHE_DAYS SCRATCH_DAYS; do
    value="${!name}"
    if ! [[ "${value}" =~ ^[1-9][0-9]{0,6}$ ]]; then
      echo "✗ prune: ${name}='${value}' must be a whole number from 1 to 9999999" >&2
      exit 2
    fi
  done
  # Colon-separated, taken literally: no word splitting, no globbing.
  TMP_ROOTS=()
  spec="${PRUNE_TMP_ROOTS-/tmp:${TMPDIR:-}}"
  while [[ -n "${spec}" ]]; do
    r="${spec%%:*}"
    if [[ "${spec}" == *:* ]]; then spec="${spec#*:}"; else spec=""; fi
    [[ -n "${r}" ]] || continue
    if [[ "${r}" != /* ]]; then
      echo "✗ prune: runtime root '${r}' (PRUNE_TMP_ROOTS or TMPDIR) is not absolute" >&2
      exit 2
    fi
    TMP_ROOTS+=("${r}")
  done
}

if [[ -t 1 ]]; then
  C_OFF=$'\033[0m' C_B=$'\033[1m' C_DIM=$'\033[2m' C_RED=$'\033[31m'
  C_GRN=$'\033[32m' C_YEL=$'\033[33m' C_CYN=$'\033[36m'
else
  C_OFF='' C_B='' C_DIM='' C_RED='' C_GRN='' C_YEL='' C_CYN=''
fi

warn() { printf '%s⚠ prune: %s%s\n' "${C_YEL}" "$*" "${C_OFF}" >&2; }

human_kib() {
  awk -v k="$1" 'BEGIN { split("KiB MiB GiB TiB", u, " "); i = 1
    while (k >= 1024 && i < 4) { k /= 1024; i++ }
    if (i == 1) printf "%d %s", k, u[i]; else printf "%.1f %s", k, u[i] }'
}

# Size in KiB; what du cannot read is reported and left out of the count.
size_kib() {
  local out
  if ! out="$(du -sk -- "$1" 2>/dev/null)"; then
    warn "du could not read all of $1; its size is a lower bound"
  fi
  out="${out%%[!0-9]*}"
  printf '%s' "${out:-0}"
}

# PHYS: the physical path of directory $1, trailing newlines kept.
phys() {
  local r
  r="$(cd -P -- "$1" 2>/dev/null && pwd -P && printf x)" || return 1
  r="${r%x}"
  PHYS="${r%$'\n'}"
}

# CANON: $1 with its parent resolved physically. Fails when $1 is relative, its
# parent is missing, or $1 itself is a symlink.
canon() {
  local p="$1" parent
  [[ "${p}" == /?* && ! -L "${p}" ]] || return 1
  parent="${p%/*}"
  phys "${parent:-/}" || return 1
  CANON="${PHYS%/}/${p##*/}"
}

# True when $2 is strictly inside $1 through real directories only: no symlink,
# `.`, `..` or empty component on the way, the last one included.
below() {
  local root="$1" path="$2" rel comp cur
  case "${path}" in "${root}"/?*) ;; *) return 1 ;; esac
  rel="${path#"${root}"/}"
  cur="${root}"
  while :; do
    comp="${rel%%/*}"
    case "${comp}" in "" | . | ..) return 1 ;; esac
    cur="${cur}/${comp}"
    [[ ! -L "${cur}" ]] || return 1
    [[ "${rel}" == */* ]] || return 0
    rel="${rel#*/}"
  done
}

# True when anything under $1 was modified within $3 units of find test $2
# (-mmin/-mtime). A tree find cannot fully read counts as recent.
touched_within() {
  local hit
  [[ -e "$1" || -L "$1" ]] || return 1
  if ! hit="$(find "$1" "$2" "-$3" -print -quit 2>/dev/null)"; then
    warn "find could not read all of $1; treating it as in use"
    return 0
  fi
  [[ -n "${hit}" ]]
}

# safe_rm PATH ROOT — remove PATH only when it lies strictly inside the physical
# directory ROOT through real directories (see below).
safe_rm() {
  local path="${1:-}" root="${2:-}"
  if [[ "${path}" != /?* || "${root}" != /?* ]]; then
    echo "✗ prune: refusing to remove '${path}' under '${root}': both must be absolute" >&2
    return 1
  fi
  if ! phys "${root}" || [[ "${PHYS}" != "${root}" ]]; then
    echo "✗ prune: refusing to remove under '${root}': not a physical directory" >&2
    return 1
  fi
  if [[ -n "${HOME:-}" ]] && phys "${HOME}" && [[ "${PHYS}" == "${root}" ]]; then
    echo "✗ prune: refusing to remove under \$HOME itself" >&2
    return 1
  fi
  if ! below "${root}" "${path}"; then
    echo "✗ prune: refusing to remove '${path}': not inside '${root}' through real directories" >&2
    return 1
  fi
  [[ -e "${path}" ]] || return 0
  rm -rf -- "${path}"
  if [[ -e "${path}" || -L "${path}" ]]; then
    echo "✗ prune: '${path}' is still there after rm" >&2
    return 1
  fi
}

# NUL-delimited lists live in files: a command substitution drops NULs.
tmpf() { TMPF="$(mktemp "${SCRATCH}/l.XXXXXX")"; }
# entries <dir>: TMPF lists every entry of dir, dotfiles and `..x` included.
entries() { tmpf; find "$1" -mindepth 1 -maxdepth 1 -print0 >"${TMPF}"; }

# The first line of file $1, whitespace removed, in LINE; fails if unreadable.
first_line() {
  LINE=""
  [[ -f "$1" && -r "$1" ]] || return 1
  IFS= read -r LINE <"$1" || :  # a last line without a newline still lands in LINE
  LINE="${LINE//[[:space:]]/}"
}

# ── The plan ───────────────────────────────────────────────────────────────
P_KIND=() P_PATH=() P_ROOT=() P_KIB=() P_WHY=() P_CTX=() P_FLAG=()
KEPT=0

plan_remove() {
  P_KIND+=("$1") P_ROOT+=("$2") P_PATH+=("$3") P_KIB+=("$4") P_WHY+=("$5") P_CTX+=("$6") P_FLAG+=("${7:-}")
  printf '  %s- remove%s %10s  %s %s(%s)%s\n' "${C_RED}" "${C_OFF}" "$(human_kib "$4")" \
    "$3" "${C_DIM}" "$5" "${C_OFF}"
}

plan_keep() {
  KEPT=$((KEPT + 1))
  printf '  %s= keep  %s %10s  %s %s— %s%s\n' "${C_GRN}" "${C_OFF}" "${3:-}" "$1" \
    "${C_DIM}" "$2" "${C_OFF}"
}

section() { printf '\n%s▶ %s%s\n' "${C_B}${C_CYN}" "$*" "${C_OFF}"; }

# ── Worktrees ──────────────────────────────────────────────────────────────
# B_*: every cargo/rustc/make outside this script's own ancestry, and its cwd.
scan_builds() {
  B_PID=() B_NAME=() B_CWD=() CWD_UNKNOWN=""
  local chain=" " pid=$$ ppid name comm cwd table
  while [[ "${pid}" =~ ^[0-9]+$ && "${pid}" -ge 1 ]]; do
    chain="${chain}${pid} "
    [[ "${pid}" -gt 1 ]] || break
    ppid="$(ps -o ppid= -p "${pid}" 2>/dev/null | tr -d ' ')" || break
    pid="${ppid}"
  done
  table="$(ps -axo pid=,comm=)"
  while read -r pid comm; do
    [[ -n "${pid}" ]] || continue
    name="${comm##*/}"
    case "${name}" in cargo | cargo-* | rustc | rustdoc | clippy-driver | make | gmake) ;; *) continue ;; esac
    case "${chain}" in *" ${pid} "*) continue ;; esac
    cwd="$(pid_cwd "${pid}")"
    if [[ -z "${cwd}" ]]; then
      if [[ ! -r /proc/self/cwd ]] && ! command -v lsof >/dev/null 2>&1; then
        CWD_UNKNOWN="cannot see build processes' cwd (no /proc, no lsof)"
      fi
      continue
    fi
    if [[ ! -d "${cwd}" ]]; then
      CWD_UNKNOWN="the cwd of ${name} (pid ${pid}) does not resolve"
      continue
    fi
    B_PID+=("${pid}") B_NAME+=("${name}") B_CWD+=("${cwd}")
  done <<<"${table}"
}

# The listed worktree that most specifically contains $1 (a submodule or plain
# subdirectory belongs to the worktree around it), or nothing.
owner_of() {
  local p="$1" w best=""
  for w in "${WTS[@]}"; do
    case "${p}" in "${w}" | "${w}"/*) [[ "${#w}" -le "${#best}" ]] || best="${w}" ;; esac
  done
  printf '%s' "${best}"
}

# list_builds <wt>: TMPF lists, NUL-delimited, the build dirs `make clean` empties there.
list_builds() {
  tmpf
  bash "${ROOT}/scripts/check-target-hygiene.sh" --list "$1" >"${TMPF}"
}

# Why worktree $1 must be kept whole, or nothing. Uses the last scan_builds.
worktree_hold() {
  local wt="$1" flag="$2" st n tree i d
  if [[ "${flag}" == prunable || ! -d "${wt}" ]]; then echo "missing on disk"; return; fi
  if [[ "${flag}" == locked ]]; then echo "locked (git worktree lock)"; return; fi
  if ! st="$(git -C "${wt}" status --porcelain --ignore-submodules=none --untracked-files=normal 2>&1)"; then
    echo "git status failed: ${st}"
    return
  fi
  if [[ -n "${st}" ]]; then echo "uncommitted changes ($(printf '%s\n' "${st}" | wc -l | tr -d ' ') path(s))"; return; fi
  if ! n="$(git -C "${wt}" rev-list --count HEAD --not --remotes 2>&1)"; then echo "git rev-list failed: ${n}"; return; fi
  if [[ "${n}" -gt 0 ]]; then
    if ! { [[ -n "${BASE_TREE}" ]] &&
      tree="$(git -C "${wt}" merge-tree --write-tree "${BASE_REF}" HEAD 2>/dev/null)" &&
      [[ "${tree%%$'\n'*}" == "${BASE_TREE}" ]]; }; then
      echo "${n} unpushed commit(s), not merged into ${BASE_REF}"
      return
    fi
  fi
  if [[ -n "${CWD_UNKNOWN}" ]]; then echo "${CWD_UNKNOWN}"; return; fi
  for i in "${!B_PID[@]}"; do
    if [[ "$(owner_of "${B_CWD[${i}]}")" == "${wt}" ]]; then
      echo "running build: ${B_NAME[${i}]} (pid ${B_PID[${i}]})"
      return
    fi
  done
  if ! list_builds "${wt}"; then echo "its build dirs could not be listed"; return; fi
  while IFS= read -r -d '' d <&3; do
    if touched_within "${d}" -mmin "${BUSY_MIN}"; then echo "${d} written in the last ${BUSY_MIN} min"; return; fi
  done 3<"${TMPF}"
}

# Why worktree $1's build dirs must stay: a live daemon runs a binary built there.
daemon_hold() {
  local pids
  pids="$(REPO_ROOT="$1" shux_daemon_pids | tr '\n' ' ')"
  [[ -z "${pids// /}" ]] || echo "a live shux daemon runs its build (pid ${pids% })"
}

# Why path $2 in worktree $1 must stay for git's sake, or nothing.
tracked_hold() {
  local out
  if ! out="$(git -C "$1" ls-files -- ":(literal)${2#"$1"/}" 2>&1)"; then
    echo "git cannot say whether it is tracked"
  elif [[ -n "${out}" ]]; then
    echo "tracked by git"
  fi
}

# Why a planned per-worktree path must stay, beyond worktree_hold.
item_hold() {
  local kind="$1" wt="$2" path="$3" h
  if ! below "${wt}" "${path}"; then echo "reached through a symlink"; return; fi
  h="$(tracked_hold "${wt}" "${path}")"
  if [[ -n "${h}" ]]; then echo "${h}"; return; fi
  case "${kind}" in
    build) daemon_hold "${wt}" ;;
    inventory) if touched_within "${path}" -mmin "${BUSY_MIN}"; then echo "written in the last ${BUSY_MIN} min"; fi ;;
    scratch) if touched_within "${path}" -mtime "${SCRATCH_DAYS}"; then echo "used in the last ${SCRATCH_DAYS} days"; fi ;;
  esac
}

plan_worktree() {
  local wt="$1" flag="$2" d h kib e out inv recent=0 builds
  inv="${wt}/.shux/out/test-inventory"
  if ! list_builds "${wt}"; then
    plan_keep "${wt}" "check-target-hygiene.sh --list failed (above); its build dirs are left alone"
    HYGIENE_FAILED=$((HYGIENE_FAILED + 1))
  else
    builds="${TMPF}"
    h="$(daemon_hold "${wt}")"
    if [[ -n "${h}" ]]; then
      plan_keep "${wt}" "build dirs kept: ${h}"
    else
      while IFS= read -r -d '' d <&3; do
        [[ "${d}" != "${inv}/target" ]] || continue
        h="$(item_hold build "${wt}" "${d}")"
        if [[ -n "${h}" ]]; then plan_keep "${d}" "${h}"; continue; fi
        kib="$(size_kib "${d}")"
        if [[ "${kib}" -gt 0 ]]; then plan_remove build "${wt}" "${d}" "${kib}" "build output" "${wt}" "${flag}"; fi
      done 3<"${builds}"
    fi
  fi
  out="${wt}/.shux/out"
  if [[ -L "${wt}/.shux" || -L "${out}" ]]; then plan_keep "${out}" "a symlink on the way; left alone"; return; fi
  [[ -d "${out}" ]] || return 0
  if [[ -e "${inv}" || -L "${inv}" ]]; then
    h="$(item_hold inventory "${wt}" "${inv}")"
    if [[ -n "${h}" ]]; then
      plan_keep "${inv}" "test-inventory cache: ${h}"
    else
      kib="$(size_kib "${inv}")"
      if [[ "${kib}" -gt $((CACHE_MIB * 1024)) ]]; then
        plan_remove inventory "${wt}" "${inv}" "${kib}" \
          "test-inventory cache over $(human_kib $((CACHE_MIB * 1024)))" "${wt}" "${flag}"
      elif ! touched_within "${inv}" -mtime "${CACHE_DAYS}"; then
        plan_remove inventory "${wt}" "${inv}" "${kib}" "test-inventory cache unused for ${CACHE_DAYS} days" \
          "${wt}" "${flag}"
      else
        plan_keep "${inv}" "test-inventory cache within limits" "$(human_kib "${kib}")"
      fi
    fi
  fi
  entries "${out}"
  while IFS= read -r -d '' e <&3; do
    [[ "${e}" != "${inv}" ]] || continue
    h="$(item_hold scratch "${wt}" "${e}")"
    if [[ "${h}" == "used in the last ${SCRATCH_DAYS} days" ]]; then
      recent=$((recent + 1))
    elif [[ -n "${h}" ]]; then
      plan_keep "${e}" "${h}"
    else
      plan_remove scratch "${wt}" "${e}" "$(size_kib "${e}")" "scratch unused for ${SCRATCH_DAYS} days" "${wt}" "${flag}"
    fi
  done 3<"${TMPF}"
  [[ "${recent}" -eq 0 ]] || plan_keep "${out}" "${recent} scratch entr(ies) used in the last ${SCRATCH_DAYS} days"
}

plan_worktrees() {
  section "Worktrees of ${ROOT} (build output, test-inventory, .shux/out scratch)"
  local list field i hold flags=()
  WTS=()
  tmpf
  list="${TMPF}"
  git -C "${ROOT}" worktree list --porcelain -z >"${list}"
  while IFS= read -r -d '' field <&3; do
    case "${field}" in
      "worktree "*)
        if canon "${field#worktree }"; then
          WTS+=("${CANON}") flags+=("")
        else
          WTS+=("${field#worktree }") flags+=(symlink)
        fi
        ;;
      locked | "locked "*) [[ "${flags[${#flags[@]} - 1]}" == symlink ]] || flags[${#flags[@]} - 1]=locked ;;
      prunable | "prunable "*) [[ "${flags[${#flags[@]} - 1]}" == symlink ]] || flags[${#flags[@]} - 1]=prunable ;;
    esac
  done 3<"${list}"
  scan_builds
  for i in "${!WTS[@]}"; do
    if [[ "${flags[${i}]}" == symlink ]]; then
      plan_keep "${WTS[${i}]}" "worktree path is a symlink or cannot be resolved"
      continue
    fi
    hold="$(worktree_hold "${WTS[${i}]}" "${flags[${i}]}")"
    if [[ -n "${hold}" ]]; then
      plan_keep "${WTS[${i}]}" "${hold}"
    else
      printf '  %s%s%s %s(clean, pushed or merged, idle)%s\n' "${C_B}" "${WTS[${i}]}" "${C_OFF}" "${C_DIM}" "${C_OFF}"
      plan_worktree "${WTS[${i}]}" "${flags[${i}]}"
    fi
  done
}

# ── vt-neutrality cache ────────────────────────────────────────────────────
NEUTRAL_LOCKED=""

# Why cache $1 must be left alone, or nothing. vt-neutrality.sh registers a
# reader marker under the lock; prune holds the same lock while it removes.
neutral_hold() {
  local cr="$1" f
  if [[ "${NEUTRAL_LOCKED}" != "${cr}" ]] && [[ -e "${cr}/lock" || -L "${cr}/lock" ]]; then
    echo "a vt-neutrality run (or another prune) holds ${cr}/lock"
    return
  fi
  if [[ -L "${cr}/bin" || -L "${cr}/readers" ]]; then echo "a symlink on the way"; return; fi
  [[ -d "${cr}/readers" ]] || return 0
  entries "${cr}/readers"
  while IFS= read -r -d '' f <&3; do
    if ! first_line "${f}"; then echo "reader marker ${f##*/} cannot be read"; return; fi
    if ! [[ "${LINE}" =~ ^[0-9]+$ ]]; then echo "reader marker ${f##*/} names no pid"; return; fi
    if ps -p "${LINE}" >/dev/null 2>&1; then echo "a vt-neutrality run is reading it (pid ${LINE})"; return; fi
  done 3<"${TMPF}"
}

plan_neutrality() {
  section "vt-neutrality base-build cache"
  local cr u i kib total=0 h units=() kibs=() hot=""
  if [[ ! -e "${NEUTRALITY}" && ! -L "${NEUTRALITY}" ]]; then echo "  (none at ${NEUTRALITY})"; return; fi
  if ! canon "${NEUTRALITY}" || [[ ! -d "${CANON}" ]]; then
    plan_keep "${NEUTRALITY}" "a symlink or not a directory; left alone"
    return
  fi
  cr="${CANON}"
  h="$(neutral_hold "${cr}")"
  if [[ -n "${h}" ]]; then plan_keep "${cr}" "${h}"; return; fi
  if [[ -d "${cr}/bin" ]]; then
    entries "${cr}/bin"
    while IFS= read -r -d '' u <&3; do units+=("${u}"); done 3<"${TMPF}"
  fi
  for u in "${cr}/target" "${cr}/fuzz-target"; do
    [[ ! -e "${u}" && ! -L "${u}" ]] || units+=("${u}")
  done
  [[ "${#units[@]}" -gt 0 ]] || { echo "  (empty)"; return; }
  for u in "${units[@]}"; do
    if touched_within "${u}" -mmin "${BUSY_MIN}"; then hot="${u}"; fi
    kib="$(size_kib "${u}")"
    kibs+=("${kib}")
    total=$((total + kib))
  done
  if [[ -n "${hot}" ]]; then plan_keep "${cr}" "in use: ${hot} written in the last ${BUSY_MIN} min"; return; fi
  for i in "${!units[@]}"; do
    u="${units[${i}]}"
    if ! below "${cr}" "${u}"; then
      plan_keep "${u}" "a symlink; left alone"
    elif [[ "${total}" -gt $((CACHE_MIB * 1024)) ]]; then
      plan_remove neutral "${cr}" "${u}" "${kibs[${i}]}" \
        "cache is $(human_kib "${total}"), over $(human_kib $((CACHE_MIB * 1024)))" "${cr}"
    elif ! touched_within "${u}" -mtime "${CACHE_DAYS}"; then
      plan_remove neutral "${cr}" "${u}" "${kibs[${i}]}" "unused for ${CACHE_DAYS} days" "${cr}"
    else
      plan_keep "${u}" "used in the last ${CACHE_DAYS} days" "$(human_kib "${kibs[${i}]}")"
    fi
  done
}

neutral_release() {
  [[ -n "${NEUTRAL_LOCKED}" ]] || return 0
  rmdir "${NEUTRAL_LOCKED}/lock" 2>/dev/null || warn "could not release ${NEUTRAL_LOCKED}/lock; remove it"
  NEUTRAL_LOCKED=""
}

# ── Runtime dirs ───────────────────────────────────────────────────────────
# True when candidate $1 holds only a real shux/ (and ble.sh's blesh/) of ours.
only_runtime_parent() {
  local x
  entries "$1"
  while IFS= read -r -d '' x <&3; do
    case "${x##*/}" in
      shux | blesh) [[ -d "${x}" && ! -L "${x}" && -O "${x}" ]] || return 1 ;;
      *) return 1 ;;
    esac
  done 3<"${TMPF}"
}

# True when dir $1 holds only files of ours that a shux daemon writes to its runtime dir.
only_runtime_files() {
  local x
  entries "$1"
  while IFS= read -r -d '' x <&3; do
    [[ ! -d "${x}" && ! -L "${x}" && -O "${x}" ]] || return 1
    case "${x##*/}" in shux*.pid | shux*.lock | *.sock | segment-*.toml) ;; *) return 1 ;; esac
  done 3<"${TMPF}"
}

# Why runtime dir $2 (inside candidate $1) must be kept, or nothing. A pid that
# is alive keeps it, whoever owns that pid now.
runtime_hold() {
  local e="$1" rd="$2" f
  if [[ -L "${e}" || ! -d "${e}" || ! -O "${e}" ]]; then echo "no longer a runtime dir of ours"; return; fi
  if [[ "${rd}" != "${e}" ]] && ! only_runtime_parent "${e}"; then echo "holds other files"; return; fi
  if ! only_runtime_files "${rd}"; then echo "holds other files"; return; fi
  entries "${rd}"
  while IFS= read -r -d '' f <&3; do
    case "${f##*/}" in *.pid | *.lock) ;; *) continue ;; esac
    if ! first_line "${f}"; then echo "unreadable ${f##*/}"; return; fi
    if [[ -z "${LINE}" ]]; then
      [[ "${f}" == *.lock ]] && continue  # start locks are empty
      echo "empty ${f##*/}"
      return
    fi
    if ! [[ "${LINE}" =~ ^[0-9]+$ ]]; then echo "${f##*/} names no pid"; return; fi
    if ps -p "${LINE}" >/dev/null 2>&1; then echo "live process (pid ${LINE}, ${f##*/})"; return; fi
  done 3<"${TMPF}"
  if touched_within "${e}" -mmin "${BUSY_MIN}"; then echo "written in the last ${BUSY_MIN} min"; fi
}

plan_runtime() {
  section "shux runtime dirs in ${TMP_ROOTS[*]:-(no roots)}"
  local seen=() r s pr e rd other=0 hold dup list
  for r in "${TMP_ROOTS[@]+"${TMP_ROOTS[@]}"}"; do
    phys "${r}" || continue
    pr="${PHYS}"
    dup=0
    for s in "${seen[@]+"${seen[@]}"}"; do [[ "${s}" != "${pr}" ]] || dup=1; done
    [[ "${dup}" -eq 0 ]] || continue
    seen+=("${pr}")
    tmpf
    list="${TMPF}"
    find "${pr}" -mindepth 1 -maxdepth 1 -name 'shux*' -print0 >"${list}"
    while IFS= read -r -d '' e <&3; do
      if [[ -L "${e}" || ! -d "${e}" || ! -O "${e}" ]]; then other=$((other + 1)); continue; fi
      if [[ -d "${e}/shux" && ! -L "${e}/shux" ]] && only_runtime_parent "${e}"; then
        rd="${e}/shux"
      elif [[ "${e##*/}" == "shux-$(id -u)" ]]; then
        rd="${e}"
      else
        other=$((other + 1))
        continue
      fi
      if ! only_runtime_files "${rd}"; then other=$((other + 1)); continue; fi
      hold="$(runtime_hold "${e}" "${rd}")"
      if [[ -n "${hold}" ]]; then
        plan_keep "${e}" "${hold}"
      else
        plan_remove runtime "${pr}" "${e}" "$(size_kib "${e}")" "no live daemon" "${rd}"
      fi
    done 3<"${list}"
  done
  [[ "${other}" -eq 0 ]] || echo "  ${C_DIM}${other} other shux* entr(ies) are not runtime dirs of ours; left alone${C_OFF}"
}

# ── Apply ──────────────────────────────────────────────────────────────────
# HOLD: why planned entry $1 must stay now, or nothing; the plan may have gone
# stale. Runs in this shell: it takes the vt-neutrality lock.
recheck() {
  local i="$1" kind="${P_KIND[$1]}" path="${P_PATH[$1]}" ctx="${P_CTX[$1]}"
  HOLD=""
  case "${kind}" in
    build | inventory | scratch)
      # A worktree's entries are contiguous: it is re-checked as a whole once, as its turn starts.
      if [[ "${i}" -eq 0 || "${P_CTX[$((i - 1))]}" != "${ctx}" ]]; then
        scan_builds
        WT_HOLD="$(worktree_hold "${ctx}" "${P_FLAG[${i}]}")"
      fi
      HOLD="${WT_HOLD}"
      [[ -n "${HOLD}" ]] || HOLD="$(item_hold "${kind}" "${ctx}" "${path}")"
      ;;
    neutral)
      if [[ "${NEUTRAL_LOCKED}" != "${ctx}" ]]; then
        neutral_release
        if ! mkdir "${ctx}/lock" 2>/dev/null; then HOLD="${ctx}/lock is held"; return; fi
        NEUTRAL_LOCKED="${ctx}"
      fi
      HOLD="$(neutral_hold "${ctx}")"
      if [[ -z "${HOLD}" ]] && touched_within "${path}" -mmin "${BUSY_MIN}"; then
        HOLD="written in the last ${BUSY_MIN} min"
      fi
      ;;
    runtime) HOLD="$(runtime_hold "${path}" "${ctx}")" ;;
  esac
}

prune_apply() {
  local i total=0 removed=0 failed=0
  for i in "${!P_PATH[@]}"; do
    recheck "${i}"
    if [[ -n "${HOLD}" ]]; then
      plan_keep "${P_PATH[${i}]}" "kept at apply: ${HOLD}"
      continue
    fi
    if safe_rm "${P_PATH[${i}]}" "${P_ROOT[${i}]}"; then
      removed=$((removed + 1))
      total=$((total + P_KIB[i]))
    else
      failed=$((failed + 1))
    fi
  done
  neutral_release
  if [[ "${failed}" -gt 0 ]]; then
    printf '%s✗ %d of %d removal(s) failed%s\n' "${C_RED}${C_B}" "${failed}" "${#P_PATH[@]}" "${C_OFF}" >&2
    return 1
  fi
  printf '%s✓ removed %d item(s), %s%s (%d kept)\n' "${C_GRN}${C_B}" "${removed}" "$(human_kib "${total}")" \
    "${C_OFF}" "${KEPT}"
}

# ── Main ───────────────────────────────────────────────────────────────────
prune_setup() {
  prune_settings
  ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
  # shellcheck disable=SC2034  # read by lib/proc_scope.sh
  REPO_ROOT="${ROOT}"
  # shellcheck source=.shux/scripts/lib/proc_scope.sh disable=SC1091
  . "${ROOT}/.shux/scripts/lib/proc_scope.sh"
  SCRATCH="$(mktemp -d "${TMPDIR:-/tmp}/shux-prune.XXXXXX")"
  if ! BASE_TREE="$(git -C "${ROOT}" rev-parse -q --verify "${BASE_REF}^{tree}")"; then
    BASE_TREE=""
    warn "${BASE_REF} is not a known ref, so no worktree counts as merged"
  fi
  HYGIENE_FAILED=0
}

prune_cleanup() {
  neutral_release
  [[ -z "${SCRATCH:-}" || ! -d "${SCRATCH}" ]] || find "${SCRATCH}" -delete
}

prune_plan() {
  plan_worktrees
  plan_neutrality
  plan_runtime
}

prune_main() {
  local apply=0 i total=0 reap rc=0
  case "$*" in
    "") ;;
    --apply) apply=1 ;;
    *) echo "usage: scripts/prune.sh [--apply]" >&2; exit 2 ;;
  esac
  prune_setup
  trap prune_cleanup EXIT
  printf '%s%s make prune%s — %s\n' "${C_B}" "$([[ ${apply} -eq 1 ]] && echo '✂' || echo '◇')" "${C_OFF}" \
    "$([[ ${apply} -eq 1 ]] && echo "removing" || echo "dry run: nothing is removed")"
  section "Daemons from this checkout (reported only; prune never stops one)"
  reap="$("${ROOT}/.shux/scripts/reap_daemons.sh" --dry-run 2>&1)" || rc=$?
  printf '%s\n' "${reap}"
  [[ "${rc}" -eq 0 ]] || { echo "✗ prune: reap_daemons.sh --dry-run failed (exit ${rc})" >&2; exit 1; }
  [[ "${reap}" != *"would be stopped"* ]] ||
    printf '  if they are leaked, stop them as a separate step: %smake reap%s\n' "${C_B}${C_CYN}" "${C_OFF}"

  prune_plan
  printf '\n'
  if [[ "${apply}" -eq 1 ]]; then
    prune_apply || rc=1
  else
    for i in "${!P_PATH[@]}"; do total=$((total + P_KIB[i])); done
    printf '%s◇ would remove %d item(s), %s%s (%d kept)\n' "${C_B}" "${#P_PATH[@]}" "$(human_kib "${total}")" \
      "${C_OFF}" "${KEPT}"
    [[ "${#P_PATH[@]}" -eq 0 ]] || printf '  apply with: %smake prune APPLY=1%s\n' "${C_B}${C_CYN}" "${C_OFF}"
  fi
  if [[ "${HYGIENE_FAILED}" -gt 0 ]]; then
    printf '%s✗ %d worktree(s) could not list their build dirs; see above%s\n' "${C_RED}${C_B}" \
      "${HYGIENE_FAILED}" "${C_OFF}" >&2
    rc=1
  fi
  return "${rc}"
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  prune_main "$@"
fi
