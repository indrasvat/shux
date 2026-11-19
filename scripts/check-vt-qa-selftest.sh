#!/usr/bin/env bash
# scripts/check-vt-qa-selftest.sh — prove the VT QA trigger can still FAIL.
#
# `scripts/check-vt-qa.sh` decides whether a diff owes SOLID-QA evidence. A
# trigger that has stopped triggering reports "no evidence required" forever and
# looks exactly like a clean run, so the only way to trust its PASS is to watch
# it fail on a diff that owes evidence and pass on empty input.
#
# Every case runs the real guard against a throwaway repository, so this asserts
# the shipped script rather than a copy of its logic.

set -euo pipefail

GUARD="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/check-vt-qa.sh"
[[ -x "$GUARD" || -f "$GUARD" ]] || {
    echo "✗ cannot find check-vt-qa.sh next to this script" >&2
    exit 1
}

# ── Never touch the caller's repository ─────────────────────────────────────
#
# Git exports repository-local variables to its hooks, and `GIT_DIR` /
# `GIT_INDEX_FILE` win over `git -C <dir>`: `-C` changes the working directory,
# not the repository git resolves to. Run from the pre-push hook (which is
# exactly where `make check-vt-qa` runs), every command below would have
# operated on the CALLER's git directory instead of the throwaway one — the
# `git add -A` and `git commit` here replaced the branch tree with this
# `README.md`, and the per-case `git reset` wiped the caller's index. Measured
# against a sacrificial clone: 1223 tracked files became 2, and the self-test
# still reported all eight cases green, because the guard it invoked was
# reading the repository it had just destroyed.
#
# `githooks(5)` says to clear these before invoking git in a foreign
# repository. Clearing them is also correct for the guard runs below: it must
# resolve `$work`, never the caller.
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_OBJECT_DIRECTORY \
    GIT_ALTERNATE_OBJECT_DIRECTORIES GIT_COMMON_DIR GIT_NAMESPACE \
    GIT_PREFIX GIT_QUARANTINE_PATH

work="$(mktemp -d "${TMPDIR:-/tmp}/shux-vt-qa-selftest.XXXXXX")"
# Canonicalise before comparing paths below: `rev-parse --absolute-git-dir`
# reports the PHYSICAL path, while on macOS $TMPDIR is a symlink (/var →
# /private/var) and carries a trailing slash. Comparing the two forms literally
# never matches, so the assertion refused every run on macOS — taking
# `make check-vt-qa`, and with it every local push, down with it.
work="$(cd "${work}" && pwd -P)"
trap 'rm -rf "${work}"' EXIT

git -C "$work" init -q

# Belt and braces: assert the isolation rather than trusting the unset above to
# stay complete. A future git that exports one more variable, or an edit that
# drops one from the list, has to fail LOUDLY here — the failure mode this
# guards is a silent destructive commit on the caller's branch, which the
# self-test cannot notice from the inside.
resolved="$(git -C "$work" rev-parse --absolute-git-dir)"
if [[ "$resolved" != "$work"/* ]]; then
    echo "✗ refusing to run: git in the throwaway dir resolves to $resolved" >&2
    echo "  (expected something under $work — a repository-local GIT_* variable" >&2
    echo "  leaked in from the caller and would make this script write there)" >&2
    exit 1
fi

git -C "$work" config user.email selftest@example.invalid
git -C "$work" config user.name selftest
echo base >"$work/README.md"
git -C "$work" add -A
git -C "$work" -c commit.gpgsign=false commit -qm base
base="$(git -C "$work" rev-parse HEAD)"

failures=0

# Stage `$1…` as new files in the throwaway repo and require exit code `$2`.
expect() {
    local want="$1" label="$2" got=0
    shift 2
    git -C "$work" reset -q
    local f
    for f in "$@"; do
        mkdir -p "$work/$(dirname "$f")"
        echo "// $label" >"$work/$f"
        git -C "$work" add -- "$f"
    done
    (cd "$work" && VT_QA_BASE="$base" bash "$GUARD" >/dev/null 2>&1) || got=$?
    if [[ "$got" == "$want" ]]; then
        printf '  \033[32m✓\033[0m %s (exit %s)\n' "$label" "$got"
    else
        printf '  \033[31m✗\033[0m %s: expected exit %s, got %s\n' "$label" "$want" "$got"
        failures=$((failures + 1))
    fi
}

echo "▶ VT QA guard self-test"

# Empty input must not be mistaken for a clean audit.
expect 0 "empty diff owes nothing"

# The positive control: a shipping VT source file with no evidence in the diff.
# If this ever passes, the gate has stopped gating.
expect 2 "shux-vt/src owes evidence" crates/shux-vt/src/lib.rs
expect 2 "shux-raster/src owes evidence" crates/shux-raster/src/lib.rs
expect 2 "pty capture owes evidence" crates/shux-pty/src/capture.rs
expect 2 "pane chrome layout owes evidence" crates/shux-ui/src/chrome.rs

# The exemption under test: trees that ship no cells and no pixels.
expect 0 "shux-vt/tests owes nothing" crates/shux-vt/tests/some_test.rs
expect 0 "shux-vt/benches owes nothing" crates/shux-vt/benches/some_bench.rs

# The exemption must not SUPPRESS a real trigger in the same diff.
expect 2 "src+tests still owes evidence" \
    crates/shux-vt/src/lib.rs crates/shux-vt/tests/some_test.rs

# A path outside the gated set stays outside it.
expect 0 "unrelated crate owes nothing" crates/shux-core/src/session.rs

# ── Evidence and governance ─────────────────────────────────────────────────
#
# Each case builds a fresh repository: a base commit, one shux-vt source commit
# (the audited SHA), then a complete, conforming scope. The mutation then breaks
# exactly one thing through the index, and the guard must notice.
q=.shux/qa/s
metric_json='{"status":"pass","max_pixel_diff_ratio":0,"max_mean_channel_delta":0,"actual":"a.png","expected":"b.png","diff":"d.png","pixel_diff_ratio":0,"mean_rgba_channel_delta":0,"size":[1,1]}'

# gov <exit> <label> <tier> <mutation> [needle]: a failing case must fail for
# its own reason, so its stderr must carry <needle>.
gov() {
    local want="$1" label="$2" tier="$3" mutate="$4" needle="${5:-}" got=0 log
    GOV_STAGE="${GOV_STAGE:-final}"
    repo="$(mktemp -d "$work/gov.XXXXXX")"
    git -C "$repo" init -q
    git -C "$repo" config user.email selftest@example.invalid
    git -C "$repo" config user.name selftest
    git -C "$repo" config commit.gpgsign false
    echo base >"$repo/README.md"
    mkdir -p "$repo/crates/shux-vt/src"
    echo v1 >"$repo/crates/shux-vt/src/lib.rs"
    git -C "$repo" add README.md crates
    git -C "$repo" commit -qm base
    GOV_BASE="$(git -C "$repo" rev-parse HEAD)"
    echo v2 >"$repo/crates/shux-vt/src/lib.rs"
    git -C "$repo" commit -qam "vt change"
    audited="$(git -C "$repo" rev-parse HEAD)"

    mkdir -p "$repo/$q/neutrality"
    printf 'VERDICT: PASS\n\nAudit of %s.\n' "$label" >"$repo/$q/SOLID-QA.md"
    echo '{"verdict":"approve"}' >"$repo/$q/council-design.json"
    echo '{"verdict":"approve"}' >"$repo/$q/council-impl.json"
    echo "$metric_json" >"$repo/$q/pixel-a.json"
    echo "$metric_json" >"$repo/$q/neutrality/corpus-ab-x.json"
    jq -n --arg b "$GOV_BASE" --arg h "$audited" '{tool: "scripts/vt-neutrality.sh", base: $b, head: $h,
        dirty: false, status: "pass", metrics_dir: "neutrality",
        layers: {"corpus-ab": {status: "pass", cases: 1, failed: []}}}' >"$repo/$q/neutrality.json"
    jq -n --arg t "$tier" --arg h "$audited" '{tier: $t, solid_qa_report: "SOLID-QA.md",
        screenshots: [], pixel_metrics: ["pixel-a.json"], neutrality: "neutrality.json",
        commit_under_audit: $h}
        + (if $t == "full" then {dootsabha_design: "council-design.json",
            dootsabha_implementation: "council-impl.json"} else {} end)' >"$repo/$q/evidence-manifest.json"
    git -C "$repo" add .shux
    git -C "$repo" commit -qm evidence

    # In this shell, not a subshell: a mutation may move GOV_BASE.
    cd "$repo"
    "$mutate"
    cd "$work"
    log="$repo.log"
    (cd "$repo" && VT_QA_BASE="$GOV_BASE" VT_QA_STAGE="$GOV_STAGE" bash "$GUARD" >"$log" 2>&1) || got=$?
    if [[ "$got" == "$want" && -n "$needle" ]] && ! grep -qF -- "$needle" "$log"; then
        printf '  \033[31m✗\033[0m %s: exit %s, but not for its reason (%s):\n' "$label" "$got" "$needle"
        sed 's/^/      /' "$log"
        failures=$((failures + 1))
    elif [[ "$got" == "$want" ]]; then
        printf '  \033[32m✓\033[0m %s (exit %s)\n' "$label" "$got"
    else
        printf '  \033[31m✗\033[0m %s: expected exit %s, got %s\n' "$label" "$want" "$got"
        failures=$((failures + 1))
    fi
    unset GOV_STAGE
}

# Rewrite a JSON file under the scope with a jq filter and stage it.
jq_stage() {
    local file="$q/$1" tmp
    tmp="$(mktemp)"
    jq "${@:2}" "$file" >"$tmp" && mv "$tmp" "$file"
    git add "$file"
}

untouched() { :; }
drop_council() { git rm -q "$q/council-design.json"; }
empty_council() { : >"$q/council-impl.json"; git add "$q/council-impl.json"; }
late_vt_edit() { echo v3 >crates/shux-vt/src/lib.rs; git commit -qam "after the audit"; }
no_audit_sha() { jq_stage evidence-manifest.json 'del(.commit_under_audit)'; }
no_neutrality() { jq_stage evidence-manifest.json 'del(.neutrality)'; }
dirty_neutrality() { jq_stage neutrality.json '.dirty = true'; }
fake_neutrality() { echo '{}' >"$q/neutrality.json"; git add "$q/neutrality.json"; }
# shellcheck disable=SC2016  # jq programs: $h and $b are jq variables
wrong_base() { jq_stage neutrality.json --arg h "$(git rev-parse HEAD~1)" '.base = $h'; }
uncommitted_metric() { echo '{}' >"$q/neutrality/extra.json"; }
failed_neutrality() {
    jq_stage neutrality.json '.status = "fail" | .layers["corpus-ab"].status = "fail"'
}
no_report() { git rm -q "$q/SOLID-QA.md"; }
# A council record committed in the base an audit is diffed against is an old
# council's, whatever this change's scope says.
# shellcheck disable=SC2016  # jq programs: $h and $b are jq variables
stale_council() {
    local a2
    GOV_BASE="$(git rev-parse HEAD)"
    echo v3 >crates/shux-vt/src/lib.rs
    git commit -qam "next change"
    a2="$(git rev-parse HEAD)"
    echo "Re-audited at $a2." >>"$q/SOLID-QA.md"
    git add "$q/SOLID-QA.md"
    jq_stage evidence-manifest.json --arg h "$a2" '.commit_under_audit = $h'
    jq_stage neutrality.json --arg h "$a2" --arg b "$GOV_BASE" '.head = $h | .base = $b'
}
# A neutrality run made before the last VT edit, under a re-pointed verdict.
# shellcheck disable=SC2016  # jq programs: $h and $b are jq variables
stale_neutrality() {
    local a2
    echo v3 >crates/shux-vt/src/lib.rs
    git commit -qam "after the neutrality run"
    a2="$(git rev-parse HEAD)"
    echo "Re-audited at $a2." >>"$q/SOLID-QA.md"
    git add "$q/SOLID-QA.md"
    jq_stage evidence-manifest.json --arg h "$a2" '.commit_under_audit = $h'
}

echo "▶ VT QA evidence + governance self-test"
gov 0 "conforming full-tier scope passes" full untouched
gov 0 "conforming trivial-tier scope passes without councils" trivial untouched
gov 2 "missing council record" full drop_council "council-design.json"
gov 2 "empty council record" full empty_council "is empty"
gov 2 "council record carried over from the base" full stale_council "is not part of this diff"
gov 2 "VT code changed after the audited SHA (stale PASS)" full late_vt_edit "commit_under_audit"
gov 2 "no commit_under_audit" full no_audit_sha "needs 'commit_under_audit'"
gov 2 "no neutrality report" full no_neutrality "needs 'neutrality'"
gov 2 "neutrality measured on a dirty tree" full dirty_neutrality "dirty tree"
gov 2 "neutrality report not written by the tool" full fake_neutrality "is not a scripts/vt-neutrality.sh report"
gov 2 "neutrality on the wrong base" full wrong_base "is not this branch's base"
gov 2 "neutrality older than the VT code" full stale_neutrality "neutrality head"
gov 2 "neutrality metric not committed" full uncommitted_metric "extra.json is not committed"
gov 2 "trivial tier with a moved pixel" trivial failed_neutrality "trivial tier requires"
gov 0 "full tier may carry an intended pixel change" full failed_neutrality
gov 2 "final stage needs the gate's report" full no_report "SOLID-QA.md"
GOV_STAGE=pregate gov 0 "pregate runs before the gate's report exists" full no_report
GOV_STAGE=pregate gov 2 "pregate still catches a missing council record" full drop_council "council-design.json"

if [[ "$failures" -gt 0 ]]; then
    echo "✗ VT QA guard self-test: $failures case(s) failed" >&2
    exit 1
fi
echo "✓ VT QA guard self-test: trigger fires, governance checks fail where they should"
