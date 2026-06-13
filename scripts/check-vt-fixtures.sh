#!/usr/bin/env bash
# scripts/check-vt-fixtures.sh — Validate committed VT corpus fixture metadata.

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
MANIFEST="$REPO_ROOT/.shux/fixtures/vt-corpus/rich-tui/manifest.json"
SYNTHETIC_MANIFEST="$REPO_ROOT/.shux/fixtures/vt-corpus/synthetic/manifest.json"
MIN_RAW_BYTES=1024

errors=()

add_error() {
    errors+=("$1")
}

if ! command -v jq >/dev/null 2>&1; then
    add_error "jq is required to validate VT fixture manifest"
fi

if [[ ! -f "$MANIFEST" ]]; then
    add_error "Missing VT fixture manifest: .shux/fixtures/vt-corpus/rich-tui/manifest.json"
elif command -v jq >/dev/null 2>&1; then
    if ! jq -e '.fixtures | type == "array" and length > 0' "$MANIFEST" >/dev/null 2>&1; then
        add_error "VT fixture manifest must contain a non-empty fixtures array"
    fi
    for key in cols rows font_size font fg_default bg_default cursor_policy duration_ms fixtures; do
        if ! jq -e "has(\"$key\")" "$MANIFEST" >/dev/null 2>&1; then
            add_error "VT fixture manifest missing top-level key '$key'"
        fi
    done

    while IFS=$'\t' read -r name raw expected_bytes expected_sha command rows cols; do
        if [[ "$raw" = /* || "$raw" == *".."* ]]; then
            add_error "Fixture $name has invalid raw path '$raw'"
            continue
        fi
        if [[ "$command" == *"/Users/"* ]]; then
            add_error "Fixture $name command leaks a machine-local path"
        fi

        raw_path="$REPO_ROOT/.shux/fixtures/vt-corpus/rich-tui/$raw"
        if [[ ! -f "$raw_path" ]]; then
            add_error "Fixture $name raw file missing: $raw"
            continue
        fi

        actual_bytes="$(wc -c < "$raw_path" | tr -d ' ')"
        if [[ "$actual_bytes" -lt "$MIN_RAW_BYTES" ]]; then
            add_error "Fixture $name is too small for rich-TUI replay: $actual_bytes bytes"
        fi
        if [[ "$actual_bytes" != "$expected_bytes" ]]; then
            add_error "Fixture $name byte count mismatch: manifest=$expected_bytes actual=$actual_bytes"
        fi

        actual_sha="$(shasum -a 256 "$raw_path" | awk '{print $1}')"
        if [[ "$actual_sha" != "$expected_sha" ]]; then
            add_error "Fixture $name sha256 mismatch: manifest=$expected_sha actual=$actual_sha"
        fi
        if [[ "$rows" -lt 1 || "$cols" -lt 1 ]]; then
            add_error "Fixture $name must declare positive rows/cols"
        fi
    done < <(jq -r '.fixtures[] | [.name, .raw, (.bytes|tostring), .sha256, .command, (.rows|tostring), (.cols|tostring)] | @tsv' "$MANIFEST")
fi

if [[ ! -f "$SYNTHETIC_MANIFEST" ]]; then
    add_error "Missing synthetic VT fixture manifest: .shux/fixtures/vt-corpus/synthetic/manifest.json"
elif command -v jq >/dev/null 2>&1; then
    if ! jq -e '.fixtures | type == "array" and length > 0' "$SYNTHETIC_MANIFEST" >/dev/null 2>&1; then
        add_error "Synthetic VT fixture manifest must contain a non-empty fixtures array"
    fi
    for required in resize-smoke wide-cjk grapheme-storage-current dec-special-graphics tabs-current origin-response osc-default-colors alternate-screen scroll-region sync-output; do
        if ! jq -e --arg name "$required" '.fixtures | any(.name == $name)' "$SYNTHETIC_MANIFEST" >/dev/null 2>&1; then
            add_error "Synthetic VT fixture manifest missing required fixture '$required'"
        fi
    done
fi

if [[ ${#errors[@]} -gt 0 ]]; then
    echo "VT FIXTURE CHECK FAILED:" >&2
    echo "" >&2
    for err in "${errors[@]}"; do
        echo "  - $err" >&2
    done
    echo "" >&2
    exit 2
fi

exit 0
