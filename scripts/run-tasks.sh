#!/usr/bin/env bash
#
# ╭──────────────────────────────────────────────────────────╮
# │  run-tasks.sh — shux task DAG executor                   │
# │                                                          │
# │  Walks the dependency graph in docs/PROGRESS.md and      │
# │  executes each task in a fresh Claude session.           │
# │                                                          │
# │  IMPORTANT: Run task 000 (bootstrap) manually first.     │
# │  It creates CLAUDE.md, CI, linting, and git hooks that   │
# │  all subsequent automated tasks depend on.               │
# │                                                          │
# │  After that:  ./scripts/run-tasks.sh --milestone M0      │
# ╰──────────────────────────────────────────────────────────╯
#
set -euo pipefail

# ── Configuration ────────────────────────────────────────────────────────────

PROGRESS="docs/PROGRESS.md"
TASKS_DIR="docs/tasks"
LOG_DIR=".task-runs"

STOP_ON_FAIL=true
DRY_RUN=false
VERBOSE=false
ONLY_TASK=""
RETRY_FAILED=false
MILESTONE_FILTER=""

# ── Theme ────────────────────────────────────────────────────────────────────

if [[ -t 1 ]]; then
    # Terminal supports color
    R='\033[0;31m'     # red
    G='\033[0;32m'     # green
    Y='\033[1;33m'     # yellow
    B='\033[0;34m'     # blue
    C='\033[0;36m'     # cyan
    # M='\033[0;35m' reserved for future use
    D='\033[2m'        # dim
    BD='\033[1m'       # bold
    NC='\033[0m'       # reset
    # Box drawing
    TL='╭' TR='╮' BL='╰' BR='╯' H='─' V='│'
    TICK='✓' CROSS='✗' ARROW='▸' DOT='●' WARN='⚠'
    BAR_DONE='█' BAR_TODO='░'
else
    R='' G='' Y='' B='' C='' D='' BD='' NC=''
    TL='+' TR='+' BL='+' BR='+' H='-' V='|'
    TICK='[ok]' CROSS='[FAIL]' ARROW='>' DOT='*' WARN='!'
    BAR_DONE='#' BAR_TODO='.'
fi

# ── Logging ──────────────────────────────────────────────────────────────────

info()    { printf "  %b%s%b %s\n" "${B}" "${ARROW}" "${NC}" "$*"; }
ok()      { printf "  %b%s%b %s\n" "${G}" "${TICK}" "${NC}" "$*"; }
fail()    { printf "  %b%s%b %s\n" "${R}" "${CROSS}" "${NC}" "$*"; }
warn()    { printf "  %b%s%b  %s\n" "${Y}" "${WARN}" "${NC}" "$*"; }
dimtext() { printf "  %b%s%b\n" "${D}" "$*" "${NC}"; }

header() {
    local title="$1"
    local width=60
    echo ""
    printf "  %b%s%s%b\n" "${C}" "${TL}" "$(printf '%*s' "$width" '' | tr ' ' "$H")${TR}" "${NC}"
    printf "  %b%s%b %b%s%b%*s%b%s%b\n" "${C}" "${V}" "${NC}" "${BD}" "$title" "${NC}" $(( width - ${#title} - 1 )) "" "${C}" "${V}" "${NC}"
    printf "  %b%s%s%b\n" "${C}" "${BL}" "$(printf '%*s' "$width" '' | tr ' ' "$H")${BR}" "${NC}"
}

# Progress bar: progress_bar completed total width
progress_bar() {
    local done=$1 total=$2 width=${3:-30}
    local pct=0
    (( total > 0 )) && pct=$(( done * 100 / total ))
    local filled=$(( done * width / total ))
    (( total == 0 )) && filled=0
    local empty=$(( width - filled ))
    local bar=""
    for ((i=0; i<filled; i++)); do bar+="${BAR_DONE}"; done
    for ((i=0; i<empty; i++)); do bar+="${BAR_TODO}"; done
    printf "%b%s%b%b%s%b %b%d%%%b (%d/%d)" \
        "${G}" "${bar:0:$filled}" "${NC}" "${D}" "${bar:$filled}" "${NC}" \
        "${BD}" "$pct" "${NC}" "$done" "$total"
}

timestamp() {
    date "+%H:%M:%S"
}

duration_fmt() {
    local secs=$1
    if (( secs >= 3600 )); then
        printf "%dh %dm %ds" $(( secs/3600 )) $(( (secs%3600)/60 )) $(( secs%60 ))
    elif (( secs >= 60 )); then
        printf "%dm %ds" $(( secs/60 )) $(( secs%60 ))
    else
        printf "%ds" "$secs"
    fi
}

# ── DAG parsing ──────────────────────────────────────────────────────────────

# Parse the task table from PROGRESS.md.
# Output: one line per task — ID|Task Name|Phase|Status|Depends On
parse_tasks() {
    perl -CSD -ne '
        next unless /^\|\s*(\d{3})\s*\|/;
        my @f = split /\|/, $_;
        shift @f;
        for (@f) { s/^\s+|\s+$//g; }
        print join("|", @f[0..4]), "\n";
    ' "$PROGRESS"
}

# Get the status of a single task by ID.
task_status() {
    local id="$1"
    parse_tasks | perl -F'\|' -ane "print \$F[3] if \$F[0] eq '$id'"
}

# Expand a dependency string into a flat list of zero-padded task IDs.
# Handles:  "—" → (empty)          em-dash = no deps
#           "000" → 000             single dep
#           "001, 002" → 001 002    comma-separated list
#           "001–011" → 001 .. 011  en-dash range
expand_deps() {
    local deps="$1"
    [[ "$deps" == "—" || "$deps" == "–" || "$deps" == "-" || -z "$deps" ]] && return

    echo "$deps" | perl -CSD -e '
        $_ = <STDIN>;
        chomp;
        for my $part (split /,/) {
            $part =~ s/^\s+|\s+$//g;
            if ($part =~ /^(\d{3})\s*[\x{2013}\-]\s*(\d{3})$/) {
                printf "%03d\n", $_ for ($1 .. $2);
            } else {
                print "$part\n" if $part =~ /^\d{3}$/;
            }
        }
    '
}

# Check whether all dependencies of a task are Done.
deps_satisfied() {
    local deps_str="$1"
    local dep_ids
    dep_ids=$(expand_deps "$deps_str")
    [[ -z "$dep_ids" ]] && return 0

    while read -r dep; do
        [[ -z "$dep" ]] && continue
        local st
        st=$(task_status "$dep")
        [[ "$st" != "Done" ]] && return 1
    done <<< "$dep_ids"
    return 0
}

# Find the task spec file for a given ID.
find_task_file() {
    find "$TASKS_DIR" -maxdepth 1 -name "${1}-*.md" -print -quit 2>/dev/null
}

# ── Status updates ───────────────────────────────────────────────────────────

update_progress_table() {
    local id="$1" old="$2" new="$3"
    perl -CSD -i -pe "
        if (/^\|\s*${id}\s*\|/) {
            s/\|\s*\Q${old}\E\s*\|/| ${new} |/;
        }
    " "$PROGRESS"
}

update_task_file_status() {
    local task_file="$1" new_status="$2"
    perl -i -pe "
        s/^\*\*Status:\*\*\s*.*/\*\*Status:\*\* ${new_status}/ if 1 .. /^---/;
    " "$task_file"
}

append_session_log() {
    local entry="$1"
    perl -CSD -i -pe "
        if (/^\*\(Dated entries/) {
            print \"${entry}\n\n\";
        }
    " "$PROGRESS"
}

# ── Prompt template ──────────────────────────────────────────────────────────

build_prompt() {
    local id="$1" task_file="$2"

    cat <<PROMPT
You are implementing a task for the shux terminal multiplexer project (Rust).

════════════════════════════════════════════════════════════════
 PROTOCOL — follow these phases in order
════════════════════════════════════════════════════════════════

PHASE 1 — ORIENT
  1. If CLAUDE.md exists at the repo root, read it and follow its conventions.
  2. Read the PRD: docs/PRD.md (skim; focus on sections the task references).
  3. Read the full task specification: ${task_file}

PHASE 2 — IMPLEMENT
  4. Follow every execution step in the task spec, in order.
  5. Create ALL files listed under "Files to Create".
  6. Modify ALL files listed under "Files to Modify" (exceptions in Phase 4).
  7. After each logical step, run its verification command.
  8. Commit after each logical step:
       feat(${id}): <what changed>

PHASE 3 — VERIFY
  9. Run the full test / verification suite from the task's acceptance criteria.
  10. Fix any failures. Do not proceed until ALL criteria pass.

PHASE 4 — BOOKKEEPING
  11. Update the task spec file header — change:
        **Status:** Pending  →  **Status:** Done
      (file: ${task_file})
  12. If CLAUDE.md has a "## Learnings" section and you discovered something
      non-obvious (gotchas, patterns, workarounds), append a terse one-liner.
      Skip if nothing noteworthy.
  13. Do NOT modify docs/PROGRESS.md — the runner script handles that.

PHASE 5 — SIGNAL
  14. When ALL acceptance criteria pass, output on its own line:

        TASK COMPLETE

      If you hit an unrecoverable blocker, output:

        TASK FAILED: <concise reason>

════════════════════════════════════════════════════════════════
PROMPT
}

# ── Task execution ───────────────────────────────────────────────────────────

run_task() {
    local id="$1" task_file="$2"
    local task_label
    task_label=$(basename "$task_file" .md)
    # Pretty name: strip ID prefix
    local pretty_name="${task_label#"${id}"-}"
    pretty_name="${pretty_name//-/ }"

    header "Task ${id}: ${pretty_name}"
    dimtext "spec: ${task_file}"
    dimtext "time: $(date '+%Y-%m-%d %H:%M:%S')"

    # ── Mark In Progress ──────────────────────────────────────────────
    update_progress_table "$id" "Pending" "In Progress"
    update_task_file_status "$task_file" "In Progress"
    git add "$PROGRESS" "$task_file" \
        && git commit -m "chore(${id}): start ${pretty_name}" --quiet 2>/dev/null || true
    info "Status: ${Y}In Progress${NC}"

    # ── Build prompt ──────────────────────────────────────────────────
    local prompt
    prompt=$(build_prompt "$id" "$task_file")

    local log_file
    log_file="${LOG_DIR}/${id}-${task_label}-$(date +%Y%m%d-%H%M%S).log"
    info "Log: ${log_file}"
    echo ""

    # ── Execute ───────────────────────────────────────────────────────
    local start_time rc=0
    start_time=$(date +%s)

    if $VERBOSE; then
        claude --dangerously-skip-permissions -p "$prompt" 2>&1 | tee "$log_file" || rc=$?
    else
        info "Running Claude session..."
        claude --dangerously-skip-permissions -p "$prompt" > "$log_file" 2>&1 || rc=$?
    fi

    local elapsed=$(( $(date +%s) - start_time ))
    local dur
    dur=$(duration_fmt "$elapsed")
    echo ""

    # ── Evaluate result ───────────────────────────────────────────────
    if [[ $rc -eq 0 ]] && tail -100 "$log_file" | grep -q "TASK COMPLETE"; then
        # ── Success ───────────────────────────────────────────────────
        printf "  %b%s COMPLETE%b  %b(%s)%b\n" "${G}" "${DOT}" "${NC}" "${D}" "$dur" "${NC}"

        update_progress_table "$id" "In Progress" "Done"
        # Task file status should already be updated by Claude, but ensure it
        update_task_file_status "$task_file" "Done"

        local date_str
        date_str=$(date +%Y-%m-%d)
        append_session_log "- **${date_str}** — Task ${id} (${pretty_name}) completed (${dur})"

        git add "$PROGRESS" "$task_file" \
            && git commit -m "chore(${id}): complete ${pretty_name}" --quiet 2>/dev/null || true

        return 0
    else
        # ── Failure ───────────────────────────────────────────────────
        printf "  %b%s FAILED%b    %b(exit=%d, %s)%b\n" "${R}" "${DOT}" "${NC}" "${D}" "$rc" "$dur" "${NC}"

        if tail -100 "$log_file" | grep -q "TASK FAILED"; then
            local reason
            reason=$(tail -100 "$log_file" | grep "TASK FAILED" | head -1)
            fail "$reason"
        fi
        dimtext "log: $log_file"

        update_progress_table "$id" "In Progress" "Failed"
        update_task_file_status "$task_file" "Failed"
        git add "$PROGRESS" "$task_file" \
            && git commit -m "chore(${id}): mark ${pretty_name} failed" --quiet 2>/dev/null || true

        return 1
    fi
}

# ── Dry-run: show execution order without running anything ───────────────────

dry_run_order() {
    header "Dry Run — Execution Order"
    echo ""

    # Simulate the DAG walk in-memory using an associative array for status
    declare -A sim_status
    while IFS='|' read -r id name phase status deps; do
        sim_status[$id]="$status"
    done < <(parse_tasks)

    local wave=0
    local total_shown=0

    while true; do
        # Find all tasks whose deps are satisfied in simulation
        local ready=()
        local any_pending=false

        while IFS='|' read -r id name phase status deps; do
            # Apply milestone filter
            if [[ -n "$MILESTONE_FILTER" && "$phase" != "$MILESTONE_FILTER" && "$phase" != "Bootstrap" ]]; then
                continue
            fi

            [[ "${sim_status[$id]}" != "Pending" ]] && continue
            any_pending=true

            # Check deps against simulated status
            local all_done=true
            local dep_ids
            dep_ids=$(expand_deps "$deps")
            if [[ -n "$dep_ids" ]]; then
                while read -r dep; do
                    [[ -z "$dep" ]] && continue
                    [[ "${sim_status[$dep]:-}" != "Done" ]] && all_done=false && break
                done <<< "$dep_ids"
            fi

            if $all_done; then
                ready+=("$id|$name|$phase|$deps")
            fi
        done < <(parse_tasks)

        if ! $any_pending; then
            break
        fi

        if [[ ${#ready[@]} -eq 0 ]]; then
            echo ""
            warn "Stuck — remaining tasks blocked by unmet dependencies"
            break
        fi

        ((wave++)) || true
        echo ""
        printf "  ${C}Wave %d${NC} ${D}(%d task%s)${NC}\n" "$wave" "${#ready[@]}" "$( (( ${#ready[@]} > 1 )) && echo 's' || echo '')"
        printf "  ${D}%s${NC}\n" "$(printf '%*s' 50 '' | tr ' ' '·')"

        for entry in "${ready[@]}"; do
            IFS='|' read -r id name phase deps <<< "$entry"
            local task_file
            task_file=$(find_task_file "$id")
            local dep_display="$deps"
            [[ "$deps" == "—" ]] && dep_display="${D}none${NC}"

            printf "  ${G}${ARROW}${NC} ${BD}%s${NC}  %-40s ${D}[%s]${NC}  deps: %b\n" \
                "$id" "$name" "$phase" "$dep_display"
            sim_status[$id]="Done"
            ((total_shown++)) || true
        done
    done

    echo ""
    printf "  ${D}%s${NC}\n" "$(printf '%*s' 50 '' | tr ' ' '─')"
    info "${total_shown} tasks would execute in ${wave} wave(s)"
    echo ""
}

# ── Status summary ───────────────────────────────────────────────────────────

show_status() {
    local total=0 done_n=0 pending_n=0 failed_n=0 in_progress_n=0

    while IFS='|' read -r id name phase status deps; do
        ((total++)) || true
        case "$status" in
            Done)        ((done_n++)) || true ;;
            Failed)      ((failed_n++)) || true ;;
            "In Progress") ((in_progress_n++)) || true ;;
            *)           ((pending_n++)) || true ;;
        esac
    done < <(parse_tasks)

    printf "  Progress: "
    progress_bar "$done_n" "$total" 30
    echo ""
    echo ""

    # Status breakdown
    printf "  ${G}${TICK} Done${NC}        %d\n" "$done_n"
    if (( in_progress_n > 0 )); then
        printf "  ${Y}${DOT} In Progress${NC} %d\n" "$in_progress_n"
    fi
    printf "  ${D}${DOT} Pending${NC}     %d\n" "$pending_n"
    if (( failed_n > 0 )); then
        printf "  ${R}${CROSS} Failed${NC}      %d\n" "$failed_n"
    fi
    echo ""
}

# ── Main ─────────────────────────────────────────────────────────────────────

main() {
    printf "\n"
    printf "  ${C}${TL}%s${TR}${NC}\n" "$(printf '%*s' 52 '' | tr ' ' "$H")"
    printf "  ${C}${V}${NC}  ${BD}shux task runner${NC}%*s${C}${V}${NC}\n" 35 ""
    printf "  ${C}${V}${NC}  ${D}DAG-driven execution via Claude sessions${NC}%*s${C}${V}${NC}\n" 10 ""
    printf "  ${C}${BL}%s${BR}${NC}\n" "$(printf '%*s' 52 '' | tr ' ' "$H")"
    echo ""
    dimtext "progress: ${PROGRESS}"
    dimtext "logs:     ${LOG_DIR}/"
    dimtext "started:  $(date '+%Y-%m-%d %H:%M:%S')"
    echo ""

    mkdir -p "$LOG_DIR"

    # ── Retry-failed: reset Failed → Pending ─────────────────────────
    if $RETRY_FAILED; then
        local count=0
        while IFS='|' read -r id name phase status deps; do
            if [[ "$status" == "Failed" ]]; then
                update_progress_table "$id" "Failed" "Pending"
                local tf
                tf=$(find_task_file "$id")
                [[ -n "$tf" ]] && update_task_file_status "$tf" "Pending"
                info "Reset task $id: ${R}Failed${NC} → ${D}Pending${NC}"
                ((count++)) || true
            fi
        done < <(parse_tasks)
        if (( count > 0 )); then
            git add -u \
                && git commit -m "chore: reset $count failed tasks to Pending" --quiet 2>/dev/null || true
            echo ""
        fi
    fi

    # Show current status
    show_status

    # ── Dry-run mode ──────────────────────────────────────────────────
    if $DRY_RUN; then
        dry_run_order
        exit 0
    fi

    # ── Single-task mode ──────────────────────────────────────────────
    if [[ -n "$ONLY_TASK" ]]; then
        local tf
        tf=$(find_task_file "$ONLY_TASK")
        if [[ -z "$tf" ]]; then
            fail "No spec file found for task $ONLY_TASK"
            exit 1
        fi
        run_task "$ONLY_TASK" "$tf"
        exit $?
    fi

    # ── Main DAG-walking loop ─────────────────────────────────────────
    local completed=0
    local failed=0
    local loop_start
    loop_start=$(date +%s)

    while true; do
        local found_pending=false
        local ready_id=""

        while IFS='|' read -r id name phase status deps; do
            # Milestone filter
            if [[ -n "$MILESTONE_FILTER" && "$phase" != "$MILESTONE_FILTER" && "$phase" != "Bootstrap" ]]; then
                continue
            fi

            if [[ "$status" != "Done" && "$status" != "Failed" ]]; then
                found_pending=true
                if [[ "$status" == "Pending" ]] && deps_satisfied "$deps"; then
                    ready_id="$id"
                    break
                fi
            fi
        done < <(parse_tasks)

        # ── Termination: all done ─────────────────────────────────────
        if ! $found_pending; then
            local total_dur
            total_dur=$(duration_fmt $(( $(date +%s) - loop_start )))
            echo ""
            printf "  ${C}${TL}%s${TR}${NC}\n" "$(printf '%*s' 52 '' | tr ' ' "$H")"
            printf "  ${C}${V}${NC}  ${G}${BD}ALL TASKS COMPLETE${NC}%*s${C}${V}${NC}\n" 33 ""
            printf "  ${C}${V}${NC}%*s${C}${V}${NC}\n" 52 ""
            printf "  ${C}${V}${NC}  Completed: ${G}%d${NC}%*s${C}${V}${NC}\n" "$completed" $(( 39 - ${#completed} )) ""
            if (( failed > 0 )); then
                printf "  ${C}${V}${NC}  Failed:    ${R}%d${NC}%*s${C}${V}${NC}\n" "$failed" $(( 39 - ${#failed} )) ""
            fi
            printf "  ${C}${V}${NC}  Duration:  ${D}%s${NC}%*s${C}${V}${NC}\n" "$total_dur" $(( 39 - ${#total_dur} )) ""
            printf "  ${C}${BL}%s${BR}${NC}\n" "$(printf '%*s' 52 '' | tr ' ' "$H")"
            echo ""
            exit 0
        fi

        # ── Termination: stuck ────────────────────────────────────────
        if [[ -z "$ready_id" ]]; then
            echo ""
            printf "  ${R}${TL}%s${TR}${NC}\n" "$(printf '%*s' 52 '' | tr ' ' "$H")"
            printf "  ${R}${V}${NC}  ${R}${BD}STUCK — No tasks ready${NC}%*s${R}${V}${NC}\n" 30 ""
            printf "  ${R}${BL}%s${BR}${NC}\n" "$(printf '%*s' 52 '' | tr ' ' "$H")"
            echo ""
            fail "Remaining tasks are blocked by dependencies or failures."
            echo ""
            info "Blocked tasks:"
            while IFS='|' read -r id name phase status deps; do
                if [[ "$status" != "Done" ]]; then
                    if [[ -n "$MILESTONE_FILTER" && "$phase" != "$MILESTONE_FILTER" && "$phase" != "Bootstrap" ]]; then
                        continue
                    fi
                    printf "    ${D}%s${NC}  %-35s  ${Y}%s${NC}  deps: %s\n" "$id" "$name" "$status" "$deps"
                fi
            done < <(parse_tasks)
            echo ""
            dimtext "To retry:  ./scripts/run-tasks.sh --retry-failed"
            echo ""
            exit 1
        fi

        # ── Execute ───────────────────────────────────────────────────
        local task_file
        task_file=$(find_task_file "$ready_id")

        if [[ -z "$task_file" ]]; then
            fail "No spec file for task $ready_id in $TASKS_DIR/"
            update_progress_table "$ready_id" "Pending" "Failed"
            ((failed++)) || true
            if $STOP_ON_FAIL; then exit 1; fi
            continue
        fi

        if run_task "$ready_id" "$task_file"; then
            ((completed++)) || true
            echo ""
            # Mini progress update between tasks
            show_status
        else
            ((failed++)) || true
            if $STOP_ON_FAIL; then
                echo ""
                printf "  ${D}%s${NC}\n" "$(printf '%*s' 50 '' | tr ' ' '─')"
                info "Stopped on failure. Options:"
                dimtext "  --continue-on-fail  skip and continue"
                dimtext "  --retry-failed      reset failed, retry"
                dimtext "  --only $ready_id           re-run this task"
                echo ""
                exit 1
            fi
        fi
    done
}

# ── CLI ──────────────────────────────────────────────────────────────────────

usage() {
    cat <<EOF

  ${BD}Usage:${NC} $(basename "$0") [OPTIONS]

  Walk the shux task dependency graph and execute each task
  in a fresh Claude session (clean context window each time).

  ${BD}Options:${NC}
    --dry-run            Show execution order without running
    --only ID            Run only task ID (e.g., --only 005)
    --continue-on-fail   Skip failures, keep going
    --retry-failed       Reset Failed tasks to Pending, then run
    --verbose            Stream Claude output to terminal
    --milestone PHASE    Only run tasks in a milestone (M0, M1, M2, M3)
    -h, --help           Show this help

  ${BD}Termination:${NC}
    The loop exits when EITHER:
      ${G}${TICK}${NC} All tasks are Done (or Failed with --continue-on-fail)
      ${R}${CROSS}${NC} No tasks are ready (blocked by unmet deps or failures)

  ${BD}Examples:${NC}
    ./scripts/run-tasks.sh --dry-run              # preview order
    ./scripts/run-tasks.sh --milestone M0          # just M0
    ./scripts/run-tasks.sh --only 002              # single task
    ./scripts/run-tasks.sh --retry-failed          # retry failures
    ./scripts/run-tasks.sh --continue-on-fail      # skip failures

EOF
}

while [[ $# -gt 0 ]]; do
    case $1 in
        --continue-on-fail) STOP_ON_FAIL=false; shift ;;
        --dry-run)          DRY_RUN=true; shift ;;
        --verbose)          VERBOSE=true; shift ;;
        --retry-failed)     RETRY_FAILED=true; shift ;;
        --only)             ONLY_TASK="$2"; shift 2 ;;
        --milestone)        MILESTONE_FILTER="$2"; shift 2 ;;
        -h|--help)          usage; exit 0 ;;
        *)                  fail "Unknown option: $1"; usage; exit 1 ;;
    esac
done

main
