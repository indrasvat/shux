#!/usr/bin/env bash
# Report, and with --apply remove, disk space shux development can regenerate.
# What it removes and what it never touches: docs/agents/disk.md.
#
#   scripts/prune.sh            dry run: the report, nothing changes
#   scripts/prune.sh --apply    the same plan, then removes it
#
# Every removal goes through safe_rm, which refuses an empty path, `/`, a
# symlink, `.`/`..`, and anything not strictly inside the root it was planned
# under. Sourcing this file defines the functions without running anything.
set -euo pipefail

# ── Settings ───────────────────────────────────────────────────────────────
prune_settings() {
  CACHE_MIB="${PRUNE_CACHE_MIB:-5120}"
  CACHE_DAYS="${PRUNE_CACHE_DAYS:-14}"
  SCRATCH_DAYS="${PRUNE_SCRATCH_DAYS:-7}"
  BUSY_MIN="${PRUNE_BUSY_MINUTES:-30}"
  BASE_REF="${PRUNE_BASE_REF:-origin/main}"
  NEUTRALITY="${SHUX_NEUTRALITY_CACHE:-${XDG_CACHE_HOME:-$HOME/.cache}/shux/vt-neutrality}"
  TMP_ROOTS="${PRUNE_TMP_ROOTS-/tmp ${TMPDIR:-}}"
  local name value
  for name in CACHE_MIB CACHE_DAYS SCRATCH_DAYS BUSY_MIN; do
    value="${!name}"
    if ! [[ "${value}" =~ ^[1-9][0-9]*$ ]]; then
      echo "✗ prune: ${name}='${value}' must be a positive whole number" >&2
      exit 2
    fi
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

# Size in KiB; a dir du cannot fully read is reported, and counted as what du saw.
size_kib() {
  local out
  if ! out="$(du -sk -- "$1" 2>/dev/null)"; then
    warn "du could not read all of $1; its size is a lower bound"
  fi
  out="${out%%[[:space:]]*}"
  printf '%s' "${out:-0}"
}

phys_dir() { (cd -P -- "$1" 2>/dev/null && pwd -P); }

# True when anything under $1 was modified within $2 find units (-mmin/-mtime).
# A tree find cannot fully read counts as recent: unknown is never stale.
touched_within() {
  local path="$1" test="$2" n="$3" hit
  [[ -e "${path}" ]] || return 1
  if ! hit="$(find "${path}" "${test}" "-${n}" -print -quit 2>/dev/null)"; then
    warn "find could not read all of ${path}; treating it as in use"
    return 0
  fi
  [[ -n "${hit}" ]]
}

# safe_rm PATH ROOT — remove PATH only when it is a real entry strictly inside ROOT.
safe_rm() {
  local path="${1:-}" root="${2:-}" real_root parent full base
  if [[ -z "${path}" || -z "${root}" ]]; then
    echo "✗ prune: refusing to remove: empty path or root" >&2
    return 1
  fi
  if [[ "${path}" != /* || "${root}" != /* ]]; then
    echo "✗ prune: refusing to remove ${path}: path and root must be absolute" >&2
    return 1
  fi
  base="${path##*/}"
  if [[ "${path}" == "/" || -z "${base}" || "${base}" == "." || "${base}" == ".." ]]; then
    echo "✗ prune: refusing to remove '${path}'" >&2
    return 1
  fi
  if ! real_root="$(phys_dir "${root}")" || [[ "${real_root}" == "/" ]] ||
    [[ "${real_root}" == "$(phys_dir "${HOME}")" ]]; then
    echo "✗ prune: refusing to remove under root '${root}'" >&2
    return 1
  fi
  [[ -e "${path}" || -L "${path}" ]] || return 0
  if [[ -L "${path}" ]]; then
    echo "✗ prune: refusing to remove ${path}: it is a symlink" >&2
    return 1
  fi
  if ! parent="$(phys_dir "${path%/*}")"; then
    echo "✗ prune: refusing to remove ${path}: cannot resolve its parent" >&2
    return 1
  fi
  full="${parent}/${base}"
  case "${full}" in
    "${real_root}"/?*) ;;
    *)
      echo "✗ prune: refusing to remove ${full}: outside ${real_root}" >&2
      return 1
      ;;
  esac
  rm -rf -- "${full}"
  if [[ -e "${full}" ]]; then
    echo "✗ prune: ${full} is still there after rm" >&2
    return 1
  fi
}

# ── The plan ───────────────────────────────────────────────────────────────
P_PATH=() P_ROOT=() P_KIB=() P_WHY=()
KEPT=0

plan_remove() {
  P_ROOT+=("$1") P_PATH+=("$2") P_KIB+=("$3") P_WHY+=("$4")
  printf '  %s- remove%s %10s  %s %s(%s)%s\n' "${C_RED}" "${C_OFF}" "$(human_kib "$3")" \
    "$2" "${C_DIM}" "$4" "${C_OFF}"
}

plan_keep() {
  KEPT=$((KEPT + 1))
  printf '  %s= keep  %s %10s  %s %s— %s%s\n' "${C_GRN}" "${C_OFF}" "${3:-}" "$1" \
    "${C_DIM}" "$2" "${C_OFF}"
}

section() { printf '\n%s▶ %s%s\n' "${C_B}${C_CYN}" "$*" "${C_OFF}"; }

# ── Worktrees ──────────────────────────────────────────────────────────────
# path is inside tree, and not inside a checkout nested in it.
in_tree() {
  local p="$1" t="$2" d
  case "${p}" in "${t}" | "${t}"/*) ;; *) return 1 ;; esac
  d="${p}"
  while [[ "${d}" != "${t}" ]]; do
    [[ ! -e "${d}/.git" ]] || return 1
    d="${d%/*}"
  done
}

# BUILDS: "pid name cwd" of every cargo/rustc/make not in this script's own ancestry.
scan_builds() {
  BUILDS=() CWD_BLIND=0
  local chain=" " pid=$$ ppid name comm cwd line
  while [[ -n "${pid}" && "${pid}" -gt 1 ]]; do
    chain="${chain}${pid} "
    ppid="$(ps -o ppid= -p "${pid}" 2>/dev/null | tr -d ' ')" || break
    pid="${ppid}"
  done
  local table
  table="$(ps -axo pid=,comm=)"
  while read -r pid comm; do
    [[ -n "${pid}" ]] || continue
    name="${comm##*/}"
    case "${name}" in cargo | cargo-* | rustc | rustdoc | clippy-driver | make | gmake) ;; *) continue ;; esac
    case "${chain}" in *" ${pid} "*) continue ;; esac
    cwd="$(pid_cwd "${pid}")"
    if [[ -z "${cwd}" ]]; then
      if [[ ! -r /proc/self/cwd ]] && ! command -v lsof >/dev/null 2>&1; then CWD_BLIND=1; fi
      continue
    fi
    line="${pid} ${name} ${cwd}"
    BUILDS+=("${line}")
  done <<<"${table}"
}

# Why worktree $1 must be kept, or nothing when it may be pruned.
worktree_hold() {
  local wt="$1" flag="$2" st n merged b
  if [[ "${flag}" == prunable || ! -d "${wt}" ]]; then echo "missing on disk"; return; fi
  if [[ "${flag}" == locked ]]; then echo "locked (git worktree lock)"; return; fi
  if ! st="$(git -C "${wt}" status --porcelain 2>&1)"; then echo "git status failed: ${st}"; return; fi
  if [[ -n "${st}" ]]; then
    echo "uncommitted changes ($(printf '%s\n' "${st}" | wc -l | tr -d ' ') path(s))"
    return
  fi
  if ! n="$(git -C "${wt}" rev-list --count HEAD --not --remotes 2>&1)"; then
    echo "git rev-list failed: ${n}"
    return
  fi
  if [[ "${n}" -gt 0 ]]; then
    merged=""
    if [[ -n "${BASE_TREE}" ]] &&
      merged="$(git -C "${wt}" merge-tree --write-tree "${BASE_REF}" HEAD 2>/dev/null)" &&
      [[ "${merged%%$'\n'*}" == "${BASE_TREE}" ]]; then
      :
    else
      echo "${n} unpushed commit(s), not merged into ${BASE_REF}"
      return
    fi
  fi
  if [[ "${CWD_BLIND}" -eq 1 ]]; then echo "cannot see build processes' cwd (no /proc, no lsof)"; return; fi
  for b in "${BUILDS[@]+"${BUILDS[@]}"}"; do
    local bpid="${b%% *}" rest="${b#* }"
    if in_tree "${rest#* }" "${wt}"; then echo "running build: ${rest%% *} (pid ${bpid})"; return; fi
  done
  if touched_within "${wt}/target" -mmin "${BUSY_MIN}"; then
    echo "target/ written in the last ${BUSY_MIN} min"
  fi
}

# cache_plan ROOT DIR LABEL — remove when over the size limit or unused for CACHE_DAYS.
cache_plan() {
  local root="$1" dir="$2" label="$3" kib
  [[ -d "${dir}" ]] || return 0
  if touched_within "${dir}" -mmin "${BUSY_MIN}"; then
    plan_keep "${dir}" "${label} in use (written in the last ${BUSY_MIN} min)"
    return
  fi
  kib="$(size_kib "${dir}")"
  if [[ "${kib}" -gt $((CACHE_MIB * 1024)) ]]; then
    plan_remove "${root}" "${dir}" "${kib}" "${label} over $(human_kib $((CACHE_MIB * 1024)))"
  elif ! touched_within "${dir}" -mtime "${CACHE_DAYS}"; then
    plan_remove "${root}" "${dir}" "${kib}" "${label} unused for ${CACHE_DAYS} days"
  else
    plan_keep "${dir}" "${label} within $(human_kib $((CACHE_MIB * 1024))) and used in ${CACHE_DAYS} days" \
      "$(human_kib "${kib}")"
  fi
}

plan_worktree() {
  local wt="$1" listing d kib out e name inventory recent=0
  inventory="${wt}/.shux/out/test-inventory"
  if ! listing="$(bash "${ROOT}/scripts/check-target-hygiene.sh" --list "${wt}")"; then
    plan_keep "${wt}" "check-target-hygiene.sh --list failed (above); its build dirs are left alone"
    HYGIENE_FAILED=$((HYGIENE_FAILED + 1))
    listing=""
  fi
  while IFS= read -r d; do
    [[ -n "${d}" && "${d}" != "${inventory}/target" ]] || continue
    kib="$(size_kib "${d}")"
    if [[ "${kib}" -gt 0 ]]; then plan_remove "${wt}" "${d}" "${kib}" "build output"; fi
  done <<<"${listing}"
  cache_plan "${wt}" "${inventory}" "test-inventory cache"
  out="${wt}/.shux/out"
  [[ -d "${out}" && ! -L "${out}" ]] || return 0
  for e in "${out}"/* "${out}"/.[!.]*; do
    [[ -e "${e}" || -L "${e}" ]] || continue
    name="${e##*/}"
    [[ "${name}" != test-inventory ]] || continue
    if [[ -n "$(git -C "${wt}" ls-files -- ".shux/out/${name}")" ]]; then
      plan_keep "${e}" "tracked by git"
    elif touched_within "${e}" -mtime "${SCRATCH_DAYS}"; then
      recent=$((recent + 1))
    else
      plan_remove "${wt}" "${e}" "$(size_kib "${e}")" "scratch unused for ${SCRATCH_DAYS} days"
    fi
  done
  [[ "${recent}" -eq 0 ]] || plan_keep "${out}" "${recent} scratch entr(ies) used in the last ${SCRATCH_DAYS} days"
}

plan_worktrees() {
  section "Worktrees of $(git -C "${ROOT}" rev-parse --show-toplevel) (build output, test-inventory, .shux/out scratch)"
  BASE_TREE=""
  if ! BASE_TREE="$(git -C "${ROOT}" rev-parse -q --verify "${BASE_REF}^{tree}")"; then
    BASE_TREE=""
    warn "${BASE_REF} is not a known ref, so no worktree counts as merged"
  fi
  scan_builds
  local porcelain line wts=() flags=() i hold wt
  porcelain="$(git -C "${ROOT}" worktree list --porcelain)"
  while IFS= read -r line; do
    case "${line}" in
      "worktree "*) wts+=("${line#worktree }") flags+=("") ;;
      locked*) flags[${#flags[@]} - 1]=locked ;;
      prunable*) flags[${#flags[@]} - 1]=prunable ;;
    esac
  done <<<"${porcelain}"
  for i in "${!wts[@]}"; do
    wt="$(phys_dir "${wts[${i}]}" || printf '%s' "${wts[${i}]}")"
    hold="$(worktree_hold "${wt}" "${flags[${i}]}")"
    if [[ -n "${hold}" ]]; then
      plan_keep "${wt}" "${hold}"
    else
      printf '  %s%s%s %s(clean, pushed or merged, idle)%s\n' "${C_B}" "${wt}" "${C_OFF}" "${C_DIM}" "${C_OFF}"
      plan_worktree "${wt}"
    fi
  done
}

# ── vt-neutrality cache ────────────────────────────────────────────────────
plan_neutrality() {
  section "vt-neutrality base-build cache"
  local cr="${NEUTRALITY}" total units=() u kib
  if [[ ! -d "${cr}" ]]; then echo "  (none at ${cr})"; return; fi
  cr="$(phys_dir "${cr}")"
  if [[ -e "${cr}/lock" ]]; then plan_keep "${cr}" "a vt-neutrality run holds ${cr}/lock"; return; fi
  if touched_within "${cr}" -mmin "${BUSY_MIN}"; then
    plan_keep "${cr}" "in use (written in the last ${BUSY_MIN} min)"
    return
  fi
  for u in "${cr}"/bin/* "${cr}/target" "${cr}/fuzz-target"; do
    [[ -e "${u}" ]] && units+=("${u}")
  done
  [[ "${#units[@]}" -gt 0 ]] || { echo "  (empty)"; return; }
  total="$(size_kib "${cr}")"
  for u in "${units[@]}"; do
    kib="$(size_kib "${u}")"
    if [[ "${total}" -gt $((CACHE_MIB * 1024)) ]]; then
      plan_remove "${cr}" "${u}" "${kib}" "cache is $(human_kib "${total}"), over $(human_kib $((CACHE_MIB * 1024)))"
    elif ! touched_within "${u}" -mtime "${CACHE_DAYS}"; then
      plan_remove "${cr}" "${u}" "${kib}" "unused for ${CACHE_DAYS} days"
    else
      plan_keep "${u}" "used in the last ${CACHE_DAYS} days" "$(human_kib "${kib}")"
    fi
  done
}

# ── Runtime dirs ───────────────────────────────────────────────────────────
# Why runtime dir $2 (inside candidate $1) must be kept, or nothing.
runtime_hold() {
  local e="$1" rd="$2" f pid
  for f in "${rd}"/*.pid "${rd}"/*.lock; do
    [[ -f "${f}" ]] || continue
    pid="$(head -n 1 "${f}" 2>/dev/null | tr -d '[:space:]')"
    [[ -n "${pid}" ]] || continue
    if ! [[ "${pid}" =~ ^[0-9]+$ ]]; then echo "unreadable ${f##*/}"; return; fi
    if ps -p "${pid}" >/dev/null 2>&1; then echo "live daemon (pid ${pid}, ${f##*/})"; return; fi
  done
  if touched_within "${e}" -mmin "${BUSY_MIN}"; then echo "written in the last ${BUSY_MIN} min"; fi
}

# True when dir $1 holds anything besides a runtime dir (shux/) and ble.sh's (blesh/).
has_other_entries() {
  local x
  for x in "$1"/* "$1"/.[!.]*; do
    [[ -e "${x}" || -L "${x}" ]] || continue
    case "${x##*/}" in shux | blesh) ;; *) return 0 ;; esac
  done
  return 1
}

# True when dir $1 holds only what a shux daemon writes to its runtime dir.
only_runtime_files() {
  local x
  for x in "$1"/* "$1"/.[!.]*; do
    [[ -e "${x}" || -L "${x}" ]] || continue
    [[ ! -d "${x}" && ! -L "${x}" ]] || return 1
    case "${x##*/}" in shux*.pid | shux*.lock | *.sock | segment-*.toml) ;; *) return 1 ;; esac
  done
}

plan_runtime() {
  section "shux runtime dirs in ${TMP_ROOTS:-(no roots)}"
  local seen=" " r pr e rd name other=0 hold
  for r in ${TMP_ROOTS}; do
    pr="$(phys_dir "${r}")" || continue
    case "${seen}" in *" ${pr} "*) continue ;; esac
    seen="${seen}${pr} "
    for e in "${pr}"/shux*; do
      [[ -d "${e}" && ! -L "${e}" && -O "${e}" ]] || continue
      name="${e##*/}"
      if [[ -d "${e}/shux" && ! -L "${e}/shux" ]]; then
        rd="${e}/shux"
        if has_other_entries "${e}"; then other=$((other + 1)); continue; fi
      elif [[ "${name}" == "shux-$(id -u)" ]]; then
        rd="${e}"
      else
        other=$((other + 1))
        continue
      fi
      if ! only_runtime_files "${rd}"; then
        other=$((other + 1))
        continue
      fi
      hold="$(runtime_hold "${e}" "${rd}")"
      if [[ -n "${hold}" ]]; then
        plan_keep "${e}" "${hold}"
      else
        plan_remove "${pr}" "${e}" "$(size_kib "${e}")" "no live daemon"
      fi
    done
  done
  [[ "${other}" -eq 0 ]] || echo "  ${C_DIM}${other} other shux* entr(ies) are not runtime dirs; left alone${C_OFF}"
}

# ── Main ───────────────────────────────────────────────────────────────────
prune_main() {
  local apply=0 i total=0 failed=0
  case "$#" in
    0) ;;
    1) [[ "$1" == --apply ]] || { echo "usage: scripts/prune.sh [--apply]" >&2; exit 2; }; apply=1 ;;
    *) echo "usage: scripts/prune.sh [--apply]" >&2; exit 2 ;;
  esac
  prune_settings
  ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
  # shellcheck disable=SC2034  # read by lib/proc_scope.sh
  REPO_ROOT="${ROOT}"
  # shellcheck source=.shux/scripts/lib/proc_scope.sh disable=SC1091
  . "${ROOT}/.shux/scripts/lib/proc_scope.sh"

  printf '%s%s make prune%s — %s\n' "${C_B}" "$([[ ${apply} -eq 1 ]] && echo '✂' || echo '◇')" "${C_OFF}" \
    "$([[ ${apply} -eq 1 ]] && echo "removing" || echo "dry run: nothing is removed")"
  section "Daemons from this checkout (make reap)"
  if [[ "${apply}" -eq 1 ]]; then
    "${ROOT}/.shux/scripts/reap_daemons.sh"
  else
    "${ROOT}/.shux/scripts/reap_daemons.sh" --dry-run
  fi

  HYGIENE_FAILED=0
  plan_worktrees
  plan_neutrality
  plan_runtime

  for i in "${!P_PATH[@]}"; do total=$((total + P_KIB[i])); done
  printf '\n'
  if [[ "${apply}" -eq 0 ]]; then
    printf '%s◇ would remove %d item(s), %s%s (%d kept)\n' "${C_B}" "${#P_PATH[@]}" "$(human_kib "${total}")" \
      "${C_OFF}" "${KEPT}"
    [[ "${#P_PATH[@]}" -eq 0 ]] || printf '  apply with: %smake prune APPLY=1%s\n' "${C_B}${C_CYN}" "${C_OFF}"
  else
    for i in "${!P_PATH[@]}"; do
      safe_rm "${P_PATH[${i}]}" "${P_ROOT[${i}]}" || failed=$((failed + 1))
    done
  fi
  if [[ "${HYGIENE_FAILED}" -gt 0 ]]; then
    printf '%s✗ %d worktree(s) could not list their build dirs; see above%s\n' "${C_RED}${C_B}" \
      "${HYGIENE_FAILED}" "${C_OFF}" >&2
    return 1
  fi
  [[ "${apply}" -eq 1 ]] || return 0
  if [[ "${failed}" -gt 0 ]]; then
    printf '%s✗ %d of %d removal(s) failed%s\n' "${C_RED}${C_B}" "${failed}" "${#P_PATH[@]}" "${C_OFF}" >&2
    return 1
  fi
  printf '%s✓ removed %d item(s), %s%s (%d kept)\n' "${C_GRN}${C_B}" "${#P_PATH[@]}" "$(human_kib "${total}")" \
    "${C_OFF}" "${KEPT}"
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  prune_main "$@"
fi
