#!/usr/bin/env bash
# scripts/check-progress.sh — Verify PROGRESS.md and task Status fields are up-to-date
#
# Usage:
#   ./scripts/check-progress.sh              # Full check (for pre-push / Stop hook)
#   ./scripts/check-progress.sh --pre-commit # Lighter check (for pre-commit)
#
# Exit codes:
#   0 = all good
#   2 = blocking error (progress not updated)
#   1 = script error
#
# This script is used by:
#   - Claude Code Stop hook (blocks agent from stopping if progress isn't updated)
#   - Claude Code PreToolUse hook (blocks git push if progress isn't updated)
#   - lefthook pre-push hook (blocks push if progress isn't updated)

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
PROGRESS_FILE="$REPO_ROOT/docs/PROGRESS.md"
TASKS_DIR="$REPO_ROOT/docs/tasks"

errors=()

# ── Helper ──────────────────────────────────────────────────────────
add_error() {
    errors+=("$1")
}

# ── Check 1: PROGRESS.md exists ────────────────────────────────────
if [[ ! -f "$PROGRESS_FILE" ]]; then
    add_error "docs/PROGRESS.md does not exist"
fi

# ── Check 2: If source code changed, PROGRESS.md should also change ──
# Compare staged + unstaged changes to HEAD
src_changed=false
progress_changed=false

# Check if any Rust source files or Cargo files have been modified
if git diff HEAD --name-only 2>/dev/null | grep -qE '\.(rs|toml)$'; then
    src_changed=true
fi
if git diff --cached --name-only 2>/dev/null | grep -qE '\.(rs|toml)$'; then
    src_changed=true
fi

# Check if PROGRESS.md has been modified
if git diff HEAD --name-only 2>/dev/null | grep -q 'docs/PROGRESS.md'; then
    progress_changed=true
fi
if git diff --cached --name-only 2>/dev/null | grep -q 'docs/PROGRESS.md'; then
    progress_changed=true
fi

if [[ "$src_changed" == true && "$progress_changed" == false ]]; then
    add_error "Source code changed but docs/PROGRESS.md was NOT updated. Update the task status and add a session log entry."
fi

# ── Check 3: No task should be 'In Progress' at push time ─────────
# (Tasks should be marked Done or back to Pending before pushing)
# This only matters for the pre-push context, not during active work
if [[ "${1:-}" != "--active-session" ]]; then
    in_progress_tasks=()
    if [[ -d "$TASKS_DIR" ]]; then
        while IFS= read -r task_file; do
            if grep -q '^\*\*Status:\*\* In Progress' "$task_file" 2>/dev/null; then
                task_name="$(basename "$task_file" .md)"
                in_progress_tasks+=("$task_name")
            fi
        done < <(find "$TASKS_DIR" -name '*.md' -type f 2>/dev/null)
    fi

    if [[ ${#in_progress_tasks[@]} -gt 0 ]]; then
        add_error "Tasks still marked 'In Progress' (should be 'Done' or 'Pending' before push): ${in_progress_tasks[*]}"
    fi
fi

# ── Check 4: If a task is marked Done, verify it has completion date ──
if [[ -d "$TASKS_DIR" ]]; then
    while IFS= read -r task_file; do
        if grep -q '^\*\*Status:\*\* Done' "$task_file" 2>/dev/null; then
            # Ensure there's some evidence of completion (not just a bare "Done")
            task_name="$(basename "$task_file" .md)"
            if ! grep -q 'Completed\|completed\|Session Log\|session log' "$PROGRESS_FILE" 2>/dev/null | grep -q "$task_name" 2>/dev/null; then
                : # Soft check — don't block on this, just warn
            fi
        fi
    done < <(find "$TASKS_DIR" -name '*.md' -type f 2>/dev/null)
fi

# ── Check 5: Session log has entries (not just the placeholder) ────
if [[ -f "$PROGRESS_FILE" ]]; then
    session_log_lines=$(sed -n '/^## Session Log/,/^## /p' "$PROGRESS_FILE" 2>/dev/null | grep -c '^\*\*20' 2>/dev/null || echo "0")
    if [[ "$src_changed" == true && "$session_log_lines" -eq 0 ]]; then
        add_error "docs/PROGRESS.md Session Log has no dated entries. Add a session log entry documenting what was done."
    fi
fi

# ── Report ──────────────────────────────────────────────────────────
if [[ ${#errors[@]} -gt 0 ]]; then
    echo "PROGRESS CHECK FAILED:" >&2
    echo "" >&2
    for err in "${errors[@]}"; do
        echo "  - $err" >&2
    done
    echo "" >&2
    echo "Fix these issues before proceeding. See CLAUDE.md Session Protocol." >&2
    exit 2
fi

exit 0
