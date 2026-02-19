#!/usr/bin/env bash
# scripts/bench-baseline.sh — M0 Performance Baseline
#
# Measures and records baseline metrics for M0 components.
# Run after all M0 integration tests pass.
#
# Usage: ./scripts/bench-baseline.sh

set -euo pipefail

echo "======================================================="
echo "  shux M0 Performance Baseline"
echo "======================================================="
echo ""

OUTPUT_FILE="docs/m0-baseline.txt"

echo "-- Building release..."
make release 2>&1 | tail -1
echo ""

echo "-- Build Metrics --"
echo ""

BINARY_SIZE=$(stat -f%z target/release/shux 2>/dev/null || stat -c%s target/release/shux 2>/dev/null || echo "0")
BINARY_SIZE_MB=$(echo "scale=2; $BINARY_SIZE / 1048576" | bc)
echo "Binary size: ${BINARY_SIZE_MB} MB (${BINARY_SIZE} bytes)"

echo ""
echo "-- Test Coverage --"
echo ""

echo "Running workspace tests..."
TEST_OUTPUT=$(make test 2>&1 || true)
TOTAL_TESTS=$(echo "$TEST_OUTPUT" | grep -o '[0-9]* tests run' | head -1 || echo "? tests run")
echo "Total: $TOTAL_TESTS"

echo ""
echo "-- Make Targets --"
echo ""

for target in build test lint check; do
    printf "  make %-12s " "$target:"
    if make "$target" >/dev/null 2>&1; then
        echo "OK"
    else
        echo "FAIL"
    fi
done

echo ""
echo "-- Summary --"
echo ""
echo "M0 baseline recorded at $(date -u +%Y-%m-%dT%H:%M:%SZ)"
echo "Binary: ${BINARY_SIZE_MB} MB"
echo "Tests: $TOTAL_TESTS"

cat > "$OUTPUT_FILE" << EOF
# M0 Performance Baseline
# Generated: $(date -u +%Y-%m-%dT%H:%M:%SZ)

binary_size_bytes=$BINARY_SIZE
binary_size_mb=${BINARY_SIZE_MB}
total_tests=$TOTAL_TESTS

# PRD 14.1 Targets (to be measured in M1+):
# keypress_to_render_p50_ms=8
# keypress_to_render_p99_ms=25
# pty_throughput_lines_per_sec=10000
# daemon_idle_memory_mb=80
EOF

echo ""
echo "Baseline written to $OUTPUT_FILE"
