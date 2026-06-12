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
MODE="${1:-}"

errors=()

# ── Helper ──────────────────────────────────────────────────────────
add_error() {
    errors+=("$1")
}

rel_path() {
    local path="$1"
    printf '%s\n' "${path#"$REPO_ROOT"/}"
}

task_id_from_name() {
    local task_name="$1"
    printf '%s\n' "${task_name%%-*}"
}

progress_task_status() {
    local task_id="$1"
    awk -F'|' -v task_id="$task_id" '
        function trim(value) {
            gsub(/^[ \t]+|[ \t]+$/, "", value)
            gsub(/\*\*/, "", value)
            return value
        }
        $0 ~ "^\\|[ \t]*" task_id "[ \t]*\\|" {
            print trim($5)
            exit
        }
    ' "$PROGRESS_FILE"
}

is_done_status() {
    local status="$1"
    local normalized
    normalized="$(printf '%s' "$status" | tr '[:upper:]' '[:lower:]')"
    [[ "$normalized" == "done" ]]
}

is_tracked_or_staged() {
    local rel="$1"
    git -C "$REPO_ROOT" ls-files --error-unmatch "$rel" >/dev/null 2>&1 \
        || git -C "$REPO_ROOT" ls-files --cached --error-unmatch "$rel" >/dev/null 2>&1
}

require_tracked_artifact() {
    local path="$1"
    local label="$2"
    local rel
    rel="$(rel_path "$path")"
    if [[ ! -e "$path" ]]; then
        add_error "$label is missing at $rel"
    elif ! is_tracked_or_staged "$rel"; then
        add_error "$label exists but is not tracked/staged: $rel"
    fi
}

require_jq() {
    if ! command -v jq >/dev/null 2>&1; then
        add_error "jq is required to validate VT QA evidence manifests and pixel metrics"
        return 1
    fi
}

manifest_path_to_abs() {
    local qa_dir="$1"
    local manifest_path="$2"
    if [[ -z "$manifest_path" ]]; then
        return 1
    fi
    if [[ "$manifest_path" = /* || "$manifest_path" == *".."* ]]; then
        return 1
    fi
    printf '%s\n' "$qa_dir/$manifest_path"
}

require_manifest_file() {
    local task_name="$1"
    local qa_dir="$2"
    local manifest_path="$3"
    local label="$4"
    local abs_path
    if ! abs_path="$(manifest_path_to_abs "$qa_dir" "$manifest_path")"; then
        add_error "VT Task $task_name manifest has invalid $label path '$manifest_path'"
        return
    fi
    require_tracked_artifact "$abs_path" "VT Task $task_name manifest $label artifact"
}

check_vt_qa_artifacts() {
    local task_name="$1"
    local qa_dir="$REPO_ROOT/.shux/qa/$task_name"
    local qa_file="$qa_dir/SOLID-QA.md"
    local manifest_file="$qa_dir/evidence-manifest.json"
    local first_line=""

    require_tracked_artifact "$qa_file" "VT Task $task_name QA gate report"
    if [[ -f "$qa_file" ]]; then
        first_line="$(head -n 1 "$qa_file" 2>/dev/null || true)"
        if [[ "$first_line" != "VERDICT: PASS" ]]; then
            add_error "VT Task $task_name QA gate report must start exactly with 'VERDICT: PASS'"
        fi
    fi

    require_tracked_artifact "$manifest_file" "VT Task $task_name evidence manifest"
    if [[ -f "$manifest_file" ]]; then
        require_jq || return
        for key in task solid_qa_report dootsabha_design dootsabha_implementation screenshots pixel_metrics; do
            if ! jq -e "has(\"$key\")" "$manifest_file" >/dev/null 2>&1; then
                add_error "VT Task $task_name evidence manifest is missing required top-level key '$key'"
            fi
        done
        if ! jq -e --arg task "$task_name" '.task == $task' "$manifest_file" >/dev/null 2>&1; then
            add_error "VT Task $task_name evidence manifest task field must equal '$task_name'"
        fi
        if ! jq -e '.screenshots | type == "array" and length > 0' "$manifest_file" >/dev/null 2>&1; then
            add_error "VT Task $task_name evidence manifest screenshots must be a non-empty array"
        fi
        if ! jq -e '.pixel_metrics | type == "array" and length > 0' "$manifest_file" >/dev/null 2>&1; then
            add_error "VT Task $task_name evidence manifest pixel_metrics must be a non-empty array"
        fi
        if ! jq -e '.screenshots | any(.[]; test("(^|[-_])actual\\.png$"))' "$manifest_file" >/dev/null 2>&1; then
            add_error "VT Task $task_name evidence manifest must reference at least one *-actual.png screenshot"
        fi

        local manifest_path artifact_path
        for key in solid_qa_report dootsabha_design dootsabha_implementation; do
            manifest_path="$(jq -r ".$key // empty" "$manifest_file")"
            require_manifest_file "$task_name" "$qa_dir" "$manifest_path" "$key"
        done
        while IFS= read -r manifest_path; do
            require_manifest_file "$task_name" "$qa_dir" "$manifest_path" "screenshot"
        done < <(jq -r '.screenshots[]? // empty' "$manifest_file")
        while IFS= read -r manifest_path; do
            require_manifest_file "$task_name" "$qa_dir" "$manifest_path" "pixel metric"
            if artifact_path="$(manifest_path_to_abs "$qa_dir" "$manifest_path")" && [[ -f "$artifact_path" ]]; then
                if ! jq -e '.status == "pass"' "$artifact_path" >/dev/null 2>&1; then
                    add_error "VT Task $task_name pixel metric $(rel_path "$artifact_path") did not pass (.status != \"pass\")"
                fi
                if ! jq -e '(.max_pixel_diff_ratio == 0) and (.max_mean_channel_delta == 0)' "$artifact_path" >/dev/null 2>&1; then
                    add_error "VT Task $task_name pixel metric $(rel_path "$artifact_path") must use exact thresholds 0/0"
                fi
            fi
        done < <(jq -r '.pixel_metrics[]? // empty' "$manifest_file")
    fi

    if [[ ! -d "$qa_dir" ]]; then
        return
    fi

    local png_count json_count tracked_png_count tracked_json_count rel
    png_count=0
    json_count=0
    tracked_png_count=0
    tracked_json_count=0
    while IFS= read -r artifact; do
        rel="$(rel_path "$artifact")"
        png_count=$((png_count + 1))
        if is_tracked_or_staged "$rel"; then
            tracked_png_count=$((tracked_png_count + 1))
        fi
    done < <(find "$qa_dir" -type f -name '*.png' 2>/dev/null)
    while IFS= read -r artifact; do
        rel="$(rel_path "$artifact")"
        json_count=$((json_count + 1))
        if is_tracked_or_staged "$rel"; then
            tracked_json_count=$((tracked_json_count + 1))
        fi
    done < <(find "$qa_dir" -type f -name '*.json' 2>/dev/null)

    if [[ "$png_count" -eq 0 || "$tracked_png_count" -eq 0 ]]; then
        add_error "VT Task $task_name has no tracked PNG evidence under .shux/qa/$task_name"
    fi
    if [[ "$json_count" -lt 2 || "$tracked_json_count" -lt 2 ]]; then
        add_error "VT Task $task_name must include tracked evidence-manifest.json plus tracked pixel metric JSON under .shux/qa/$task_name"
    fi
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
if [[ "$MODE" != "--active-session" ]]; then
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
        task_name="$(basename "$task_file" .md)"
        task_id="$(task_id_from_name "$task_name")"
        task_file_done=false
        progress_done=false
        progress_status=""
        vt_quality_task=false
        if grep -q '^\*\*Status:\*\* Done' "$task_file" 2>/dev/null; then
            task_file_done=true
        fi
        if grep -q 'Milestone:\*\* VT Quality Track' "$task_file" 2>/dev/null; then
            vt_quality_task=true
        fi
        progress_status="$(progress_task_status "$task_id")"
        if is_done_status "$progress_status"; then
            progress_done=true
        fi

        if [[ "$task_file_done" == true || ( "$vt_quality_task" == true && "$progress_done" == true ) ]]; then
            if [[ "$task_file_done" != "$progress_done" ]]; then
                if [[ "$vt_quality_task" == true ]]; then
                    add_error "VT Task $task_name status mismatch: task file Done=$task_file_done, docs/PROGRESS.md status='${progress_status:-missing}'"
                fi
            fi
            # Ensure there's some evidence of completion (not just a bare "Done")
            if ! grep -q 'Completed\|completed\|Session Log\|session log' "$PROGRESS_FILE" 2>/dev/null | grep -q "$task_name" 2>/dev/null; then
                : # Soft check — don't block on this, just warn
            fi

            # VT Quality Gate check
            if [[ "$vt_quality_task" == true ]]; then
                check_vt_qa_artifacts "$task_name"
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
