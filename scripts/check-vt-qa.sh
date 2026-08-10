#!/usr/bin/env bash
# scripts/check-vt-qa.sh — require VT QA evidence from any diff that touches VT
# rendering, and require nothing from any diff that does not.
#
# Usage:
#   ./scripts/check-vt-qa.sh
#   VT_QA_BASE=origin/main ./scripts/check-vt-qa.sh
#
# Exit codes:
#   0 = nothing owed, or evidence present and conforming
#   1 = the guard could not determine what to check (fail loud, never fail open)
#   2 = evidence is missing or non-conforming
#
# ── Why this reads the diff and not a task file ──────────────────────────────
#
# The previous version of this gate keyed off a `**Quality Gate:** shux-vt-solid-qa`
# marker inside `docs/tasks/NNN-*.md`. Enforcement therefore followed what an
# author remembered to write about their own change rather than what the change
# actually touched, and across the eleven tasks before issue #123 exactly one was
# gated — a smoke detector wired to a switch you flip yourself. Both halves of the
# contract now come from the diff:
#
#   trigger     — the diff touches a path that produces cells or pixels
#   requirement — the SAME diff adds or updates a `.shux/qa/<scope>/` folder
#
# Nothing walks history. A finished audit is never revalidated, and a diff that
# touches no VT code is asked for nothing.
#
# `<scope>` is free-form: it is whatever folder name the audit chose. There is no
# derivation from a task number and no `.task ==` assertion, because that coupling
# is the thing being removed.

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
cd "$REPO_ROOT"

# Paths that own what ends up on screen. A change inside any of these can move a
# cell or a pixel, which is exactly what the VT gate audits.
#
# The `shux-ui` entries are the composition half of the snapshot pipeline, not
# the attach UI: `window.snapshot` renders through `shux_ui::compose` before it
# reaches `shux_raster`, so `composed.rs` and everything it pulls in to build
# that grid — borders, cell buffer, viewport offsets, crossterm→VT conversion —
# changes snapshot pixels directly. They were missing from the first version of
# this list and a `composed.rs`-only diff sailed through with "no evidence
# required". Input, keys, copy mode, help overlay and status-bar CONTENT stay
# out: those are the TUI gate's surface per the table in CLAUDE.md.
#
# Add a path here when a new surface starts producing cells or pixels. Over-
# triggering costs one audit; under-triggering is how the old gate got to 1-in-11.
VT_PATHS=(
    'crates/shux-vt/'
    'crates/shux-raster/'
    'crates/shux-pty/src/capture.rs'
    'crates/shux-ui/src/composed.rs'
    'crates/shux-ui/src/compositor.rs'
    'crates/shux-ui/src/borders.rs'
    'crates/shux-ui/src/buffer.rs'
    'crates/shux-ui/src/viewport.rs'
    'crates/shux-ui/src/vt_convert.rs'
)

errors=()
add_error() { errors+=("$1"); }

# The index, not the worktree: staged counts, untracked does not. Evidence that
# is not at least staged cannot land with the diff it is meant to justify.
is_tracked() {
    git ls-files --error-unmatch -- "$1" >/dev/null 2>&1
}

# ── Resolve the base to diff against ────────────────────────────────────────
#
# A guard that cannot find its base must say so and stop. Silently treating an
# unresolvable base as "no changes" would turn every misconfigured run green,
# which is the exact failure mode this issue is about.
resolve_base() {
    local candidate
    if [[ -n "${VT_QA_BASE:-}" ]]; then
        if ! git rev-parse --verify --quiet "${VT_QA_BASE}^{commit}" >/dev/null; then
            echo "✗ VT_QA_BASE='${VT_QA_BASE}' does not resolve to a commit." >&2
            echo "  Fetch it first, or unset VT_QA_BASE to fall back to origin/main." >&2
            exit 1
        fi
        git merge-base "${VT_QA_BASE}" HEAD
        return
    fi
    for candidate in origin/main main; do
        if git rev-parse --verify --quiet "${candidate}^{commit}" >/dev/null; then
            git merge-base "${candidate}" HEAD 2>/dev/null && return
        fi
    done
    echo "✗ Cannot resolve a base commit to diff against (tried origin/main, main)." >&2
    echo "  Set VT_QA_BASE=<ref> explicitly." >&2
    exit 1
}

base="$(resolve_base)"

# Committed range plus the index and worktree, so evidence staged alongside the
# change counts before it is pushed.
changed="$(
    {
        git diff --name-only "$base" HEAD
        git diff --name-only HEAD
        git diff --cached --name-only
    } | sort -u
)"

# ── Trigger ─────────────────────────────────────────────────────────────────
touched_vt=()
while IFS= read -r file; do
    [[ -n "$file" ]] || continue
    for prefix in "${VT_PATHS[@]}"; do
        if [[ "$file" == "$prefix"* ]]; then
            touched_vt+=("$file")
            break
        fi
    done
done <<<"$changed"

if [[ ${#touched_vt[@]} -eq 0 ]]; then
    echo "✓ VT QA: diff touches no VT rendering paths — no evidence required"
    exit 0
fi

# ── Requirement: a verdict issued for THIS diff ─────────────────────────────
#
# A scope counts only when the diff touches BOTH its report and its manifest.
# Selecting a scope from any changed file underneath it was a hole wide enough
# to drive the whole gate through: appending one newline to
# `.shux/qa/067-shux-vt-resize-reflow/SOLID-QA.md` let an unrelated shux-vt
# change inherit task 067's months-old metrics and councils, and the guard said
# PASS. The verdict has to be issued for the change in front of it.
scopes=()
add_scope_if_complete() {
    local scope="$1"
    [[ " ${scopes[*]-} " != *" $scope "* ]] || return 0
    grep -qxF ".shux/qa/$scope/SOLID-QA.md" <<<"$changed" || return 0
    grep -qxF ".shux/qa/$scope/evidence-manifest.json" <<<"$changed" || return 0
    scopes+=("$scope")
}
while IFS= read -r file; do
    [[ "$file" == .shux/qa/*/* ]] || continue
    scope="${file#.shux/qa/}"
    scope="${scope%%/*}"
    [[ -n "$scope" ]] || continue
    add_scope_if_complete "$scope"
done <<<"$changed"

if [[ ${#scopes[@]} -eq 0 ]]; then
    {
        echo "VT QA CHECK FAILED:"
        echo ""
        echo "  This diff touches VT rendering paths:"
        printf '    - %s\n' "${touched_vt[@]}"
        echo ""
        echo "  but adds or updates no .shux/qa/<scope>/ evidence. Run the"
        echo "  shux-vt-solid-qa gate and commit BOTH of its files:"
        echo ""
        echo "    .shux/qa/<scope>/SOLID-QA.md            first line exactly 'VERDICT: PASS'"
        echo "    .shux/qa/<scope>/evidence-manifest.json see .shux/qa/README.md"
        echo ""
        echo "  Both must be in this diff: a scope is not selected by touching some"
        echo "  other file underneath it, or an old audit would satisfy a new change."
        echo "  <scope> is free-form — name it after the change, not after a task."
    } >&2
    exit 2
fi

if ! command -v jq >/dev/null 2>&1; then
    echo "✗ jq is required to validate VT QA evidence manifests." >&2
    exit 1
fi

# Resolves a manifest-relative artifact path into RESOLVED and reports problems.
# Deliberately a global rather than stdout: `add_error` inside a command
# substitution runs in a subshell and its findings are silently discarded.
# Non-whitespace content this branch ADDS to a file, or empty if it adds none.
#
# Deliberately a command substitution and not `… | grep -q`: `grep -q` exits the
# moment it matches, the upstream `sed` takes SIGPIPE, and under `set -o pipefail`
# the pipeline reports 141 — a FAILURE — precisely when the content was found.
# That inverted this check on any report long enough for grep to short-circuit,
# which is every real one. Caught by the positive control, not by review.
added_content() {
    git diff "$base" -- "$1" | sed -n 's/^+\([^+]\)/\1/p' | tr -d '[:space:]'
}

RESOLVED=""
resolve_artifact() {
    local scope="$1" qa_dir="$2" manifest_path="$3" label="$4" rel
    RESOLVED=""
    case "$manifest_path" in
        "" | /* | *..*)
            add_error "$scope: manifest has an invalid $label path '$manifest_path'"
            return 1
            ;;
    esac
    rel="$qa_dir/$manifest_path"
    if [[ ! -e "$rel" ]]; then
        add_error "$scope: manifest $label artifact is missing: $rel"
        return 1
    fi
    if ! is_tracked "$rel"; then
        add_error "$scope: manifest $label artifact is not tracked: $rel"
        return 1
    fi
    RESOLVED="$rel"
}

check_scope() {
    local scope="$1"
    local qa_dir=".shux/qa/$scope"
    local qa_file="$qa_dir/SOLID-QA.md"
    local manifest="$qa_dir/evidence-manifest.json"
    local key manifest_path expected metric_rel
    local baseline_committed=false

    if [[ ! -d "$qa_dir" ]]; then
        add_error "$scope: .shux/qa/$scope/ was in the diff but no longer exists"
        return
    fi

    if [[ ! -f "$qa_file" ]]; then
        add_error "$scope: missing $qa_file"
    elif ! is_tracked "$qa_file"; then
        add_error "$scope: $qa_file is not tracked"
    elif [[ "$(head -n 1 "$qa_file")" != "VERDICT: PASS" ]]; then
        add_error "$scope: $qa_file must start exactly with 'VERDICT: PASS'"
    elif [[ -z "$(added_content "$qa_file")" ]]; then
        # Requiring the file to appear in the diff is not enough on its own —
        # `echo "" >> SOLID-QA.md` appears in the diff. The report has to say
        # something new about this change. This raises the floor; it is not
        # fraud-proof, and is not trying to be.
        add_error "$scope: $qa_file is in the diff but gains no new content — the verdict must be written for this change, not inherited"
    fi

    if [[ ! -f "$manifest" ]]; then
        add_error "$scope: missing $manifest"
        return
    fi
    if ! is_tracked "$manifest"; then
        add_error "$scope: $manifest is not tracked"
    fi
    if ! jq -e . "$manifest" >/dev/null 2>&1; then
        add_error "$scope: $manifest is not valid JSON"
        return
    fi

    # `task` is deliberately NOT required, and no field is compared against the
    # folder name. That assertion was the task coupling.
    for key in solid_qa_report dootsabha_design dootsabha_implementation screenshots pixel_metrics; do
        jq -e "has(\"$key\")" "$manifest" >/dev/null 2>&1 ||
            add_error "$scope: manifest is missing required top-level key '$key'"
    done

    jq -e '.screenshots | type == "array"' "$manifest" >/dev/null 2>&1 ||
        add_error "$scope: manifest screenshots must be an array"
    jq -e '.pixel_metrics | type == "array" and length > 0' "$manifest" >/dev/null 2>&1 ||
        add_error "$scope: manifest pixel_metrics must be a non-empty array"

    for key in solid_qa_report dootsabha_design dootsabha_implementation; do
        manifest_path="$(jq -r ".$key // empty" "$manifest")"
        resolve_artifact "$scope" "$qa_dir" "$manifest_path" "$key" || true
    done

    while IFS= read -r manifest_path; do
        resolve_artifact "$scope" "$qa_dir" "$manifest_path" "screenshot" || true
    done < <(jq -r '.screenshots[]? // empty' "$manifest")

    while IFS= read -r manifest_path; do
        resolve_artifact "$scope" "$qa_dir" "$manifest_path" "pixel metric" || continue
        metric_rel="$RESOLVED"
        if ! jq -e '.status == "pass"' "$metric_rel" >/dev/null 2>&1; then
            add_error "$scope: pixel metric $metric_rel did not pass (.status != \"pass\")"
        fi
        if ! jq -e '(.max_pixel_diff_ratio == 0) and (.max_mean_channel_delta == 0)' "$metric_rel" >/dev/null 2>&1; then
            add_error "$scope: pixel metric $metric_rel must use exact thresholds 0/0"
        fi
        # A committed baseline is what makes an `-actual.png` reviewable: with no
        # baseline in the repo there is nothing to compare it against, and
        # committing it anyway contradicts "no screenshots committed unless
        # justified as durable baselines". So the PNG requirement is derived from
        # whether a metric names a baseline this repo actually tracks, rather than
        # demanded unconditionally.
        expected="$(jq -r '.expected // empty' "$metric_rel" 2>/dev/null || true)"
        if [[ -n "$expected" ]] && is_tracked "$expected"; then
            baseline_committed=true
        fi
    done < <(jq -r '.pixel_metrics[]? // empty' "$manifest")

    if [[ "$baseline_committed" == true ]] &&
        ! jq -e '.screenshots | any(.[]; test("(^|[-_])actual\\.png$"))' "$manifest" >/dev/null 2>&1; then
        add_error "$scope: a pixel metric compares against a committed baseline, so the manifest must reference at least one *-actual.png screenshot"
    fi
}

for scope in "${scopes[@]}"; do
    check_scope "$scope"
done

if [[ ${#errors[@]} -gt 0 ]]; then
    {
        echo "VT QA CHECK FAILED:"
        echo ""
        printf '  - %s\n' "${errors[@]}"
        echo ""
        echo "See .shux/qa/README.md for the evidence contract."
    } >&2
    exit 2
fi

echo "✓ VT QA: ${#touched_vt[@]} VT path(s) touched; evidence conforming in ${scopes[*]}"
