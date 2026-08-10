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
    # `compose()` writes StatusBar::render_row() cells straight into the composed
    # grid, so this file rasterizes. The earlier "status bar is TUI-gate" split
    # was about what the bar SAYS, not how it lands in pixels.
    'crates/shux-ui/src/statusbar.rs'
    # Pane geometry: where every pane's cells land in the frame.
    'crates/shux-core/src/layout.rs'
    # The comparator that produces the 0/0 metrics this gate trusts. CLAUDE.md:
    # the correctness rule "applies hardest to defects in verification machinery".
    '.claude/automations/pixel_verify.py'
)
#
# Known limit, stated rather than papered over: a `vte` bump in `Cargo.lock`
# swaps the escape-sequence parser without touching any path above. Gating every
# `Cargo.lock` change would fire on every unrelated dependency bump — exactly the
# friction issue #123 exists to remove — so this is left to review.

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
    local candidate merge_base
    if [[ -n "${VT_QA_BASE:-}" ]]; then
        if ! git rev-parse --verify --quiet "${VT_QA_BASE}^{commit}" >/dev/null; then
            echo "✗ VT_QA_BASE='${VT_QA_BASE}' does not resolve to a commit." >&2
            echo "  Fetch it first, or unset VT_QA_BASE to fall back to origin/main." >&2
            exit 1
        fi
        # Captured, not inlined: with unrelated histories `git merge-base` prints
        # nothing and exits 1, which `set -e` turned into a silent exit — the one
        # thing the header of this file promises never happens.
        if ! merge_base="$(git merge-base "${VT_QA_BASE}" HEAD 2>/dev/null)" ||
            [[ -z "$merge_base" ]]; then
            echo "✗ VT_QA_BASE='${VT_QA_BASE}' shares no history with HEAD, so there is" >&2
            echo "  no diff to judge. Fetch the real base, or point VT_QA_BASE at it." >&2
            exit 1
        fi
        printf '%s\n' "$merge_base"
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

# The committed range plus the INDEX. The worktree is deliberately excluded from
# both halves, so "staged counts, untracked does not" holds symmetrically.
# Folding unstaged edits in broke parity with CI in both directions: a stray
# uncommitted debug line inside `crates/shux-vt/` blocked a docs-only push, and
# unstaged evidence green-lit a push that CI then rejected. `pre-push` must judge
# what is being pushed.
#
# `-z` is not a style choice. `git diff --name-only` QUOTES any path containing
# non-ASCII, a tab, a quote or a backslash — `"crates/shux-vt/src/grid\303\251.rs"`
# — and a quoted line matches no prefix, so such a file was invisible to the
# trigger. Worse, `core.quotePath` is user configuration, so a contributor could
# get a different verdict from CI on the same tree. `--no-renames` is likewise
# load-bearing: git reports only a rename's DESTINATION, so moving a VT file out
# of the gated tree (and editing it in the same commit) erased it from the diff.
changed_file="$(mktemp "${TMPDIR:-/tmp}/shux-vt-qa.XXXXXX")"
trap 'rm -f "${changed_file}"' EXIT
{
    git diff -z --no-renames --name-only "$base" HEAD
    git diff -z --no-renames --cached --name-only
} | sort -zu >"$changed_file"

changed_files=()
while IFS= read -r -d '' file; do
    [[ -n "$file" ]] && changed_files+=("$file")
done <"$changed_file"

changed_contains() {
    local needle="$1" file
    for file in ${changed_files[@]+"${changed_files[@]}"}; do
        [[ "$file" == "$needle" ]] && return 0
    done
    return 1
}

# ── Trigger ─────────────────────────────────────────────────────────────────
#
# A file entry also matches its module-directory form: `composed.rs` and
# `composed/mod.rs` are the same unit, and the ordinary `foo.rs` → `foo/mod.rs`
# refactor would otherwise drop a surface off this gate permanently.
touched_vt=()
for file in ${changed_files[@]+"${changed_files[@]}"}; do
    for prefix in "${VT_PATHS[@]}"; do
        if [[ "$file" == "$prefix" || "$file" == "$prefix"* || "$file" == "${prefix%.rs}/"* ]]; then
            touched_vt+=("$file")
            break
        fi
    done
done

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
    changed_contains ".shux/qa/$scope/SOLID-QA.md" || return 0
    changed_contains ".shux/qa/$scope/evidence-manifest.json" || return 0
    scopes+=("$scope")
}
for file in ${changed_files[@]+"${changed_files[@]}"}; do
    [[ "$file" == .shux/qa/*/* ]] || continue
    scope="${file#.shux/qa/}"
    scope="${scope%%/*}"
    [[ -n "$scope" ]] || continue
    add_scope_if_complete "$scope"
done

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

# Non-whitespace content this branch ADDS to a file, or empty if it adds none.
#
# `--cached` so this matches the trigger's view: the committed range plus the
# index, never the unstaged worktree.
#
# Deliberately a command substitution and not `… | grep -q`: `grep -q` exits the
# moment it matches, the upstream `sed` takes SIGPIPE, and under `set -o pipefail`
# the pipeline reports 141 — a FAILURE — precisely when the content was found.
# That inverted this check on any report long enough for grep to short-circuit,
# which is every real one. Caught by the positive control, not by review.
added_content() {
    git diff --cached "$base" -- "$1" | sed -n 's/^+\([^+]\)/\1/p' | tr -d '[:space:]'
}

# Resolves a manifest-relative artifact path into RESOLVED and reports problems.
# Deliberately a global rather than stdout: `add_error` inside a command
# substitution runs in a subshell and its findings are silently discarded.
RESOLVED=""
resolve_artifact() {
    local scope="$1" qa_dir="$2" manifest_path="$3" label="$4" rel component
    RESOLVED=""
    # Reject `..` as a PATH COMPONENT, not as a substring: `v1..v2-diff.png` is a
    # perfectly ordinary screenshot name and the substring test refused it.
    if [[ -z "$manifest_path" || "$manifest_path" == /* ]]; then
        add_error "$scope: manifest has an invalid $label path '$manifest_path'"
        return 1
    fi
    while IFS= read -r component; do
        if [[ "$component" == ".." ]]; then
            add_error "$scope: manifest $label path escapes the scope: '$manifest_path'"
            return 1
        fi
    done < <(tr '/' '\n' <<<"$manifest_path")
    rel="$qa_dir/$manifest_path"
    # `-L` before `-f`: a tracked SYMLINK satisfied both `-e` and `git ls-files`
    # while pointing anywhere — including outside the repository, where a
    # reviewer sees a dangling link and CI reads whatever sits at that path.
    # `-f` (not `-e`) also stops a directory standing in for a council record.
    if [[ -L "$rel" ]]; then
        add_error "$scope: manifest $label artifact is a symlink, so its content is not in this repo: $rel"
        return 1
    fi
    if [[ ! -f "$rel" ]]; then
        add_error "$scope: manifest $label artifact is missing or not a regular file: $rel"
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
    # Strip a UTF-8 BOM and a trailing CR before comparing. An editor writing
    # CRLF, or any tool that prepends a BOM, otherwise turns an honest
    # `VERDICT: PASS` into a rejection that names the one thing that is correct.
    elif [[ "$(head -n 1 "$qa_file" | sed $'1s/^\xef\xbb\xbf//; s/\r$//')" != "VERDICT: PASS" ]]; then
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
    elif [[ -z "$(added_content "$manifest")" ]]; then
        add_error "$scope: $manifest is in the diff but gains no new content — the evidence list must describe this change"
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

    # Every artifact reference must be a STRING. Pre-#123 folders store objects
    # and prose here, and `jq -r` renders those as pretty-printed JSON which was
    # then used as a filesystem path — one such manifest produced 134 KB of error
    # output with whole JSON objects quoted as "paths". Fail once, legibly.
    if ! jq -e '.screenshots | type == "array" and all(.[]; type == "string")' "$manifest" >/dev/null 2>&1; then
        add_error "$scope: manifest screenshots must be an array of path strings"
        return
    fi
    if ! jq -e '.pixel_metrics | type == "array" and length > 0 and all(.[]; type == "string")' "$manifest" >/dev/null 2>&1; then
        add_error "$scope: manifest pixel_metrics must be a non-empty array of path strings"
        return
    fi
    for key in solid_qa_report dootsabha_design dootsabha_implementation; do
        if ! jq -e "has(\"$key\") and (.[\"$key\"] | type == \"string\")" "$manifest" >/dev/null 2>&1; then
            add_error "$scope: manifest '$key' must be a path string relative to $qa_dir"
            continue
        fi
        manifest_path="$(jq -r ".$key" "$manifest")"
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
        # Provenance: the contract says these come from pixel_verify.py, but any
        # tracked JSON with a `status` field used to pass — a manifest could even
        # name ITSELF as its own pixel metric. Assert the comparator's shape, the
        # same fields scripts/check-tui-qa.sh requires.
        if ! jq -e 'has("actual") and has("expected") and has("diff") and has("pixel_diff_ratio") and has("mean_rgba_channel_delta") and (.size | type == "array" and length == 2)' "$metric_rel" >/dev/null 2>&1; then
            add_error "$scope: pixel metric $metric_rel does not look like .claude/automations/pixel_verify.py output"
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
