#!/usr/bin/env bash
# Print how many CPUs this process may ACTUALLY use.
#
# `getconf _NPROCESSORS_ONLN` counts the CPUs the machine has, which is not the
# same question. Inside a container with a CPU quota it over-reports, sometimes
# wildly: a 64-core host handing out a 2-CPU quota still reports 64, and
# `make test` would multiply that by four and launch 256 test processes onto two
# CPUs. That is precisely the starvation this suite's scheduling exists to avoid
# — a daemon-backed test blows its wall-clock budget and fails having done
# nothing wrong.
#
# Three sources, and the answer is the smallest of them:
#
#   * `nproc` — respects CPU affinity (`taskset`, cpuset), not quota.
#   * cgroup v2 `cpu.max`  — "<quota> <period>", or "max" for unlimited.
#   * cgroup v1 `cpu.cfs_quota_us` / `cpu.cfs_period_us` — -1 for unlimited.
#
# macOS has neither cgroup file and falls through to the CPU count, which is
# correct there.
set -euo pipefail

online="$( (command -v nproc >/dev/null 2>&1 && nproc) \
  || getconf _NPROCESSORS_ONLN 2>/dev/null \
  || sysctl -n hw.ncpu 2>/dev/null \
  || echo 1 )"

# Guard against a non-numeric or zero answer rather than letting it flow into
# arithmetic downstream, where it becomes a syntax error and an empty `-j`.
case "${online}" in
  '' | *[!0-9]*) online=1 ;;
esac
[ "${online}" -ge 1 ] || online=1

quota_cpus=""

# cgroup v2
if [ -r /sys/fs/cgroup/cpu.max ]; then
  read -r q p < /sys/fs/cgroup/cpu.max || true
  if [ "${q:-max}" != "max" ] && [ -n "${p:-}" ] && [ "${p}" -gt 0 ] 2>/dev/null; then
    # Round UP: a 1.5-CPU quota should permit 2, not 1.
    quota_cpus=$(( (q + p - 1) / p ))
  fi
fi

# cgroup v1
if [ -z "${quota_cpus}" ] && [ -r /sys/fs/cgroup/cpu/cpu.cfs_quota_us ]; then
  q="$(cat /sys/fs/cgroup/cpu/cpu.cfs_quota_us 2>/dev/null || echo -1)"
  p="$(cat /sys/fs/cgroup/cpu/cpu.cfs_period_us 2>/dev/null || echo 0)"
  if [ "${q}" -gt 0 ] 2>/dev/null && [ "${p}" -gt 0 ] 2>/dev/null; then
    quota_cpus=$(( (q + p - 1) / p ))
  fi
fi

if [ -n "${quota_cpus}" ] && [ "${quota_cpus}" -ge 1 ] && [ "${quota_cpus}" -lt "${online}" ]; then
  printf '%s\n' "${quota_cpus}"
else
  printf '%s\n' "${online}"
fi
