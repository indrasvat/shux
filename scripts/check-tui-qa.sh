#!/usr/bin/env bash
# Verify committed general TUI QA evidence manifests.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
QA_ROOT="$REPO_ROOT/.shux/qa"

errors=()

add_error() {
    errors+=("$1")
}

rel_path() {
    local path="$1"
    printf '%s\n' "${path#$REPO_ROOT/}"
}

is_tracked_or_staged() {
    local rel="$1"
    git -C "$REPO_ROOT" ls-files --error-unmatch "$rel" >/dev/null 2>&1 ||
        git -C "$REPO_ROOT" diff --cached --name-only -- "$rel" | grep -qxF "$rel"
}

manifest_path_to_abs() {
    local qa_dir="$1"
    local manifest_path="$2"
    case "$manifest_path" in
        ""|/*|*..*)
            return 1
            ;;
        *)
            printf '%s/%s\n' "$qa_dir" "$manifest_path"
            ;;
    esac
}

require_tracked_artifact() {
    local abs="$1"
    local label="$2"
    local rel
    rel="$(rel_path "$abs")"
    if [[ ! -f "$abs" ]]; then
        add_error "$label is missing: $rel"
        return
    fi
    if ! is_tracked_or_staged "$rel"; then
        add_error "$label must be tracked or staged: $rel"
    fi
}

require_manifest_file() {
    local scope="$1"
    local qa_dir="$2"
    local manifest_path="$3"
    local label="$4"
    local abs
    if ! abs="$(manifest_path_to_abs "$qa_dir" "$manifest_path")"; then
        add_error "TUI QA $scope manifest has invalid $label path '$manifest_path'"
        return
    fi
    require_tracked_artifact "$abs" "TUI QA $scope $label artifact"
}

require_png_file() {
    local abs="$1"
    local label="$2"
    require_tracked_artifact "$abs" "$label"
    if [[ -f "$abs" ]]; then
        if [[ ! -s "$abs" ]]; then
            add_error "$label must be non-empty: $(rel_path "$abs")"
            return
        fi
        if ! head -c 8 "$abs" | od -A n -t x1 | tr -d ' \n' | grep -qx '89504e470d0a1a0a'; then
            add_error "$label must be a PNG file: $(rel_path "$abs")"
        fi
    fi
}

require_capture_file() {
    local abs="$1"
    local label="$2"
    require_tracked_artifact "$abs" "$label"
    if [[ -f "$abs" && ! -s "$abs" ]]; then
        add_error "$label must be non-empty: $(rel_path "$abs")"
    fi
}

manifest_count=0
required_scope="${TUI_QA_SCOPE:-}"
if [[ -n "$required_scope" && ( "$required_scope" = /* || "$required_scope" = *..* || "$required_scope" = *"/"* ) ]]; then
    add_error "TUI_QA_SCOPE must be a single safe .shux/qa/<scope> directory name"
fi
if [[ "${TUI_QA_REQUIRED:-0}" == "1" && -z "$required_scope" ]]; then
    add_error "TUI_QA_REQUIRED=1 also requires TUI_QA_SCOPE=<scope>"
fi
if [[ -n "$required_scope" && ! -f "$QA_ROOT/$required_scope/tui-evidence-manifest.json" ]]; then
    add_error "TUI_QA_SCOPE=$required_scope but .shux/qa/$required_scope/tui-evidence-manifest.json was not found"
fi

if [[ -d "$QA_ROOT" ]]; then
    while IFS= read -r manifest; do
        if [[ -n "$required_scope" && "$manifest" != "$QA_ROOT/$required_scope/tui-evidence-manifest.json" ]]; then
            continue
        fi
        if ! command -v jq >/dev/null 2>&1; then
            echo "check-tui-qa requires jq when TUI QA manifests exist" >&2
            exit 2
        fi
        manifest_count=$((manifest_count + 1))
        qa_dir="$(dirname "$manifest")"
        scope="$(basename "$qa_dir")"
        rel_manifest="$(rel_path "$manifest")"

        require_tracked_artifact "$manifest" "TUI QA $scope evidence manifest"

        for key in scope tui_qa_report screenshots captures pixel_metrics commands cleanup; do
            if ! jq -e "has(\"$key\")" "$manifest" >/dev/null 2>&1; then
                add_error "TUI QA $scope manifest is missing required top-level key '$key'"
            fi
        done

        if ! jq -e --arg scope "$scope" '.scope == $scope' "$manifest" >/dev/null 2>&1; then
            add_error "TUI QA $scope manifest scope field must equal '$scope'"
        fi
        if ! jq -e '.screenshots | type == "array" and length > 0' "$manifest" >/dev/null 2>&1; then
            add_error "TUI QA $scope manifest screenshots must be a non-empty array"
        fi
        if ! jq -e '.screenshots | all(.[]; type == "string" and test("[^[:space:]]"))' "$manifest" >/dev/null 2>&1; then
            add_error "TUI QA $scope manifest screenshots must contain non-blank paths"
        fi
        if ! jq -e '.captures | type == "array" and length > 0' "$manifest" >/dev/null 2>&1; then
            add_error "TUI QA $scope manifest captures must be a non-empty array"
        fi
        if ! jq -e '.captures | all(.[]; type == "string" and test("[^[:space:]]"))' "$manifest" >/dev/null 2>&1; then
            add_error "TUI QA $scope manifest captures must contain non-blank paths"
        fi
        if ! jq -e '.pixel_metrics | type == "array" and length > 0' "$manifest" >/dev/null 2>&1; then
            add_error "TUI QA $scope manifest pixel_metrics must be a non-empty array"
        fi
        if ! jq -e '.pixel_metrics | all(.[]; type == "string" and test("[^[:space:]]"))' "$manifest" >/dev/null 2>&1; then
            add_error "TUI QA $scope manifest pixel_metrics must contain non-blank paths"
        fi
        if ! jq -e '.commands | type == "array" and length > 0' "$manifest" >/dev/null 2>&1; then
            add_error "TUI QA $scope manifest commands must be a non-empty array"
        fi
        if ! jq -e '.commands | all(.[]; type == "string" and test("[^[:space:]]"))' "$manifest" >/dev/null 2>&1; then
            add_error "TUI QA $scope manifest commands must contain non-blank command strings"
        fi
        if ! jq -e '.cleanup.no_new_shux == true and .cleanup.no_new_orphan_automation_processes == true' "$manifest" >/dev/null 2>&1; then
            add_error "TUI QA $scope manifest cleanup must prove no new shux daemons and no new orphan automation processes"
        fi

        report_path="$(jq -r '.tui_qa_report // empty' "$manifest")"
        require_manifest_file "$scope" "$qa_dir" "$report_path" "report"
        report_abs=""
        if report_abs="$(manifest_path_to_abs "$qa_dir" "$report_path")" && [[ -f "$report_abs" ]]; then
            first_line="$(head -n 1 "$report_abs" 2>/dev/null || true)"
            if [[ "$first_line" != "VERDICT: PASS" ]]; then
                add_error "TUI QA $scope report must start exactly with 'VERDICT: PASS'"
            fi
        fi

        while IFS= read -r artifact; do
            if screenshot_abs="$(manifest_path_to_abs "$qa_dir" "$artifact")"; then
                require_png_file "$screenshot_abs" "TUI QA $scope screenshot artifact"
            else
                add_error "TUI QA $scope manifest has invalid screenshot path '$artifact'"
            fi
        done < <(jq -r '.screenshots[]? // empty' "$manifest")

        while IFS= read -r artifact; do
            if capture_abs="$(manifest_path_to_abs "$qa_dir" "$artifact")"; then
                require_capture_file "$capture_abs" "TUI QA $scope capture artifact"
            else
                add_error "TUI QA $scope manifest has invalid capture path '$artifact'"
            fi
        done < <(jq -r '.captures[]? // empty' "$manifest")

        while IFS= read -r artifact; do
            require_manifest_file "$scope" "$qa_dir" "$artifact" "pixel metric"
            metric_abs=""
            if metric_abs="$(manifest_path_to_abs "$qa_dir" "$artifact")" && [[ -f "$metric_abs" ]]; then
                if ! jq -e '.status == "pass"' "$metric_abs" >/dev/null 2>&1; then
                    add_error "TUI QA $scope pixel metric $(rel_path "$metric_abs") did not pass (.status != \"pass\")"
                fi
                if ! jq -e 'has("actual") and has("expected") and has("diff") and has("size") and has("pixel_diff_ratio") and has("mean_rgba_channel_delta") and (.size | type == "array" and length == 2)' "$metric_abs" >/dev/null 2>&1; then
                    add_error "TUI QA $scope pixel metric $(rel_path "$metric_abs") must look like .claude/automations/pixel_verify.py output"
                fi
            fi
        done < <(jq -r '.pixel_metrics[]? // empty' "$manifest")

        if ! is_tracked_or_staged "$rel_manifest"; then
            add_error "TUI QA $scope manifest must be tracked or staged: $rel_manifest"
        fi
    done < <(find "$QA_ROOT" -mindepth 2 -maxdepth 2 -name 'tui-evidence-manifest.json' -type f 2>/dev/null | sort)
fi

if [[ ${#errors[@]} -gt 0 ]]; then
    echo "TUI QA CHECK FAILED:" >&2
    echo "" >&2
    for err in "${errors[@]}"; do
        echo "  - $err" >&2
    done
    exit 1
fi

if [[ "$manifest_count" -eq 0 && "${TUI_QA_REQUIRED:-0}" == "1" && ${#errors[@]} -eq 0 ]]; then
    echo "TUI QA CHECK FAILED:" >&2
    echo "" >&2
    echo "  - TUI_QA_REQUIRED=1 but no .shux/qa/*/tui-evidence-manifest.json files were found" >&2
    exit 1
fi

if [[ "$manifest_count" -eq 0 ]]; then
    echo "✓ No general TUI QA manifests found"
else
    echo "✓ General TUI QA manifests valid: $manifest_count"
fi
