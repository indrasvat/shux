#!/usr/bin/env bash
# scripts/check-vt-corpus-scratch-selftest.sh — prove `make test-vt-corpus`
# leaves the tracked tree alone.
#
# The corpus harness writes per-case actual/diff PNGs and pixel JSON on every
# run. The diff PNGs are re-encoded each time and never byte-equal to a
# committed copy, so a harness that writes into a tracked directory dirties the
# checkout on a clean, passing run. Its scratch belongs under the gitignored
# `.shux/out/`; tracked evidence changes only through
# `make refresh-vt-corpus-evidence`.
#
# Every case runs the real `.shux/scripts/vt_corpus_check.sh` inside a
# throwaway repository, with `cargo` and `uv` stubbed so it takes seconds rather
# than a full rasterizer build. `jq` and `git` are the real ones.
#
# Cases:
#   1. default env      -> exits 0, tree clean, evidence lands in .shux/out
#   2. VT_CORPUS_QA set -> the deliberate refresh path still writes there
#   3. reintroduced bug -> qa defaulting to the tracked dir is detected

set -euo pipefail

SCRIPT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/.shux/scripts/vt_corpus_check.sh"
[[ -f "$SCRIPT" ]] || {
    echo "✗ cannot find .shux/scripts/vt_corpus_check.sh" >&2
    exit 1
}
for tool in git jq; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "✗ missing required command: $tool" >&2
        exit 1
    }
done

# Repository-local variables exported to hooks win over `git -C`; see
# check-vt-qa-selftest.sh for what that costs when this runs from pre-push.
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_OBJECT_DIRECTORY \
    GIT_ALTERNATE_OBJECT_DIRECTORIES GIT_COMMON_DIR GIT_NAMESPACE \
    GIT_PREFIX GIT_QUARANTINE_PATH
unset VT_CORPUS_FIXTURES VT_CORPUS_GOLDENS VT_CORPUS_OUT VT_CORPUS_QA

scratch="$(mktemp -d "${TMPDIR:-/tmp}/shux-vt-corpus-scratch.XXXXXX")"
scratch="$(cd "${scratch}" && pwd -P)"
trap 'rm -rf "${scratch}"' EXIT

stubs="${scratch}/stubs"
mkdir -p "${stubs}"

# `cargo run ... --out DIR`: one rendered case and its report, with absolute
# paths like the real harness emits.
cat > "${stubs}/cargo" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
out="" goldens=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --out) out="$2"; shift 2 ;;
    --goldens) goldens="$2"; shift 2 ;;
    *) shift ;;
  esac
done
[[ -n "${out}" && -n "${goldens}" ]] || { echo "stub cargo: missing --out/--goldens" >&2; exit 2; }
mkdir -p "${out}"
printf 'actual\n' > "${out}/synthetic-probe-actual.png"
jq -n --arg a "${out}/synthetic-probe-actual.png" \
      --arg e "${goldens}/synthetic-probe-expected.png" \
      --arg d "${out}/synthetic-probe-diff.png" \
  '{cases: [{layer: "synthetic", name: "probe", actual_png: $a, expected_png: $e, diff_png: $d}]}' \
  > "${out}/corpus-report.json"
STUB

# `uv run --script pixel_verify.py ACTUAL EXPECTED --diff PATH ...`: writes a
# freshly "encoded" diff, as the real one does on every run.
cat > "${stubs}/uv" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
diff=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --diff) diff="$2"; shift 2 ;;
    *) shift ;;
  esac
done
[[ -n "${diff}" ]] || { echo "stub uv: missing --diff" >&2; exit 2; }
printf 'regenerated %s\n' "$$" > "${diff}"
printf '{"pixel_diff_ratio": 0}\n'
STUB
chmod +x "${stubs}/cargo" "${stubs}/uv"

tracked_qa_rel=".shux/qa/073-shux-vt-corpus-regression-harness"

# A throwaway repo shaped like this one: the script, the gitignore that makes
# .shux/out scratch, and committed evidence the harness must not touch.
make_repo() {
    local root="$1" script="$2"
    mkdir -p "${root}/.shux/scripts" "${root}/.shux/fixtures/vt-corpus" \
        "${root}/.shux/goldens/073-vt-corpus" "${root}/${tracked_qa_rel}"
    cp "${script}" "${root}/.shux/scripts/vt_corpus_check.sh"
    printf 'out/\n' > "${root}/.shux/.gitignore"
    printf 'expected\n' > "${root}/.shux/goldens/073-vt-corpus/synthetic-probe-expected.png"
    printf 'committed\n' > "${root}/${tracked_qa_rel}/synthetic-probe-diff.png"
    printf 'committed\n' > "${root}/${tracked_qa_rel}/synthetic-probe-actual.png"
    printf '{}\n' > "${root}/${tracked_qa_rel}/evidence-manifest.json"
    git -C "${root}" init -q
    local resolved
    resolved="$(git -C "${root}" rev-parse --absolute-git-dir)"
    if [[ "${resolved}" != "${root}"/* ]]; then
        echo "✗ refusing to run: git in ${root} resolves to ${resolved}" >&2
        exit 1
    fi
    git -C "${root}" add -A
    git -C "${root}" -c user.name=selftest -c user.email=selftest@invalid \
        -c commit.gpgsign=false commit -q -m init
}

run_harness() {
    local root="$1"
    shift
    (cd "${root}" && env PATH="${stubs}:${PATH}" "$@" bash .shux/scripts/vt_corpus_check.sh) \
        > "${root}.log" 2>&1
}

dirty() {
    git -C "$1" status --porcelain --untracked-files=all
}

failures=0
fail() {
    echo "✗ $1" >&2
    if [[ -n "${2:-}" && -f "$2" ]]; then
        sed 's/^/    /' "$2" >&2
    fi
    failures=$((failures + 1))
}

# 1. Default environment: scratch only.
repo="${scratch}/default"
make_repo "${repo}" "${SCRIPT}"
if run_harness "${repo}"; then
    changed="$(dirty "${repo}")"
    if [[ -n "${changed}" ]]; then
        fail "a default run changed the tracked tree:"$'\n'"${changed}"
    elif [[ ! -s "${repo}/.shux/out/073-vt-corpus/qa/evidence-manifest.json" ]]; then
        fail "a default run wrote no evidence under .shux/out/073-vt-corpus/qa" "${repo}.log"
    else
        echo "✓ default run: tracked tree clean, evidence in .shux/out"
    fi
else
    fail "a default run exited non-zero" "${repo}.log"
fi

# 2. The deliberate refresh path still rewrites the committed evidence.
repo="${scratch}/refresh"
make_repo "${repo}" "${SCRIPT}"
if run_harness "${repo}" VT_CORPUS_QA="${repo}/${tracked_qa_rel}"; then
    if dirty "${repo}" | grep -q "${tracked_qa_rel}/synthetic-probe-diff.png"; then
        echo "✓ VT_CORPUS_QA run: committed evidence refreshed"
    else
        fail "VT_CORPUS_QA did not redirect evidence into ${tracked_qa_rel}" "${repo}.log"
    fi
else
    fail "a VT_CORPUS_QA run exited non-zero" "${repo}.log"
fi

# 3. Reintroduce the defect and prove case 1's assertion catches it.
mutant="${scratch}/vt_corpus_check.mutant.sh"
# shellcheck disable=SC2016  # the `${...}` here are the script's own text, matched and written literally
sed 's|^qa="\${VT_CORPUS_QA:-\${out}/qa}"$|qa="${VT_CORPUS_QA:-${repo_root}/'"${tracked_qa_rel}"'}"|' \
    "${SCRIPT}" > "${mutant}"
if cmp -s "${SCRIPT}" "${mutant}"; then
    fail "could not reintroduce the defect: the qa= default line changed shape; update this self-test"
else
    repo="${scratch}/mutant"
    make_repo "${repo}" "${mutant}"
    if ! run_harness "${repo}"; then
        fail "the mutant harness exited non-zero; case 3 proves nothing" "${repo}.log"
    elif [[ -n "$(dirty "${repo}")" ]]; then
        echo "✓ reintroduced defect: tracked-tree change detected"
    else
        fail "a harness writing into ${tracked_qa_rel} went undetected"
    fi
fi

if [[ "${failures}" -ne 0 ]]; then
    echo "✗ vt-corpus scratch self-test: ${failures} failure(s)" >&2
    exit 1
fi
echo "✓ vt-corpus scratch self-test passed (3 cases)"
