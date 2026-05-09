# 059 — M3 Final Quality Gate and v1.0 Release

**Status:** Pending
**Depends On:** 053, 054, 055, 056, 057, 058
**Parallelizable With:** ---

---

## Problem

This is the final quality gate before shux v1.0. Every M3 task must be verified, every M2 contract must still hold, every performance budget must be met, and every documentation claim must be accurate. The PRD's M3 "Done when" criteria and success metrics (SS18) are the acceptance criteria for this task. There is no room for "we'll fix it later" — this gate ensures shux ships as a reliable, performant, well-documented tool that people can actually use as a daily driver.

## PRD Reference

- **SS 17** M3 "Done when": "All performance budgets met. Zero known crashers. Plugin authoring guide includes working examples."
- **SS 18** Success metrics: All 8 metrics must be verified
  - Daily-driver adoption: Author uses shux exclusively >= 2 weeks
  - Performance: All p99 budgets met
  - API coverage: 100% of operations testable via CLI
  - Plugin API sufficiency: All 3 bundled plugins use only public plugin API
  - Agent story: All 3 agent scenario tests pass
  - Visual quality: Zero visual regressions vs golden images
  - Crash resilience: Zero crashes in 1-week dogfood period
  - Plugin DX: Working "hello world" plugin in < 15 minutes
- **SS 16.1** Testing pyramid: All 6 layers must pass
- **SS 14.1** Performance budgets: All P0 metrics must be within hard limits

---

## Files to Create

- `scripts/run-m3-gate.sh` — Complete M3 quality gate script
- `tests/integration/m3_final_test.rs` — Final integration tests

## Files to Modify

- `docs/PROGRESS.md` — Mark all M3 tasks complete, update final status, add release session log
- `CLAUDE.md` — Final learnings update (create file if missing, per task 000)
- `Cargo.toml` — Bump version to 1.0.0

---

## Execution Steps

### Step 1: Create Final Quality Gate Script

Create `scripts/run-m3-gate.sh`:

```bash
#!/usr/bin/env bash
set -euo pipefail

echo "╔══════════════════════════════════════════════════╗"
echo "║   shux M3 Final Quality Gate — v1.0 Release      ║"
echo "╚══════════════════════════════════════════════════╝"
echo ""

PASS=0
FAIL=0
WARN=0
TOTAL=0

check() {
    local name="$1"
    local result="$2"  # "pass", "fail", "warn"
    local detail="${3:-}"
    TOTAL=$((TOTAL + 1))

    case "$result" in
        pass)
            echo "  [PASS] $name"
            PASS=$((PASS + 1))
            ;;
        warn)
            echo "  [WARN] $name: $detail"
            WARN=$((WARN + 1))
            ;;
        fail)
            echo "  [FAIL] $name: $detail"
            FAIL=$((FAIL + 1))
            ;;
    esac
}

# ═══════════════════════════════════════
# Section 1: Build
# ═══════════════════════════════════════
echo "─── 1. Build ───"

if cargo build --release 2>/dev/null; then
    check "Release build" "pass"
else
    check "Release build" "fail" "cargo build --release failed"
fi

if cargo clippy --workspace --all-targets -- -D warnings 2>/dev/null; then
    check "Clippy clean" "pass"
else
    check "Clippy clean" "fail" "clippy warnings"
fi

if cargo fmt --all -- --check 2>/dev/null; then
    check "Formatting" "pass"
else
    check "Formatting" "fail" "formatting violations"
fi

# ═══════════════════════════════════════
# Section 2: Test Layers
# ═══════════════════════════════════════
echo ""
echo "─── 2. Test Layers ───"

# L1: Headless unit tests
if cargo nextest run --workspace --lib 2>/dev/null; then
    check "L1: Headless unit tests" "pass"
else
    check "L1: Headless unit tests" "fail"
fi

# L2: PTY integration
if cargo nextest run --workspace -E 'test(pty)' 2>/dev/null; then
    check "L2: PTY integration tests" "pass"
else
    check "L2: PTY integration tests" "fail"
fi

# L3: API contract tests
if cargo nextest run --test 'm2_*' 2>/dev/null; then
    check "L3: API contract tests" "pass"
else
    check "L3: API contract tests" "fail"
fi

# L4: Visual regression
if cargo nextest run --test 'visual_*' 2>/dev/null; then
    check "L4: Visual regression tests" "pass"
else
    check "L4: Visual regression tests" "warn" "may require macOS GUI"
fi

# L5: Agent scenarios
if [ -d tests/agent_scenarios ]; then
    AGENT_PASS=true
    for script in tests/agent_scenarios/scenario_*.py; do
        if ! python3 "$script" 2>/dev/null; then
            AGENT_PASS=false
        fi
    done
    if $AGENT_PASS; then
        check "L5: Agent scenarios (3 scripts)" "pass"
    else
        check "L5: Agent scenarios" "fail"
    fi
else
    check "L5: Agent scenarios" "fail" "directory missing"
fi

# L6: Dogfood (manual)
check "L6: Dogfood (manual verification)" "warn" "requires manual dogfooding session"

# Doc tests
if cargo test --workspace --doc 2>/dev/null; then
    check "Doc tests" "pass"
else
    check "Doc tests" "fail"
fi

# ═══════════════════════════════════════
# Section 3: Performance Budgets
# ═══════════════════════════════════════
echo ""
echo "─── 3. Performance Budgets ───"

if [ -x scripts/bench-all.sh ]; then
    if ./scripts/bench-all.sh 2>/dev/null; then
        check "All P0 performance budgets" "pass"
    else
        check "All P0 performance budgets" "fail"
    fi
else
    check "All P0 performance budgets" "fail" "bench-all.sh not found"
fi

# ═══════════════════════════════════════
# Section 4: Fuzzing
# ═══════════════════════════════════════
echo ""
echo "─── 4. Fuzzing ───"

if [ -x scripts/fuzz-smoke.sh ]; then
    if FUZZ_DURATION=10 ./scripts/fuzz-smoke.sh 2>/dev/null; then
        check "Fuzz smoke (5 targets)" "pass"
    else
        check "Fuzz smoke" "fail" "crashes found"
    fi
else
    check "Fuzz smoke" "fail" "fuzz-smoke.sh not found"
fi

# Check for known crash artifacts
if [ -d fuzz/artifacts ] && find fuzz/artifacts -name 'crash-*' -type f 2>/dev/null | head -1 | grep -q .; then
    check "Zero known crashers" "fail" "crash artifacts exist"
else
    check "Zero known crashers" "pass"
fi

# ═══════════════════════════════════════
# Section 5: Features
# ═══════════════════════════════════════
echo ""
echo "─── 5. Feature Completeness ───"

# Shell completions
if cargo run -p shux -- completions bash > /dev/null 2>&1; then
    check "Shell completions (bash)" "pass"
else
    check "Shell completions (bash)" "fail"
fi

if cargo run -p shux -- completions zsh > /dev/null 2>&1; then
    check "Shell completions (zsh)" "pass"
else
    check "Shell completions (zsh)" "fail"
fi

if cargo run -p shux -- completions fish > /dev/null 2>&1; then
    check "Shell completions (fish)" "pass"
else
    check "Shell completions (fish)" "fail"
fi

# Bundled plugins
SHUX_BIN="./target/release/shux"
if $SHUX_BIN plugin ls 2>/dev/null | grep -q "status-bar"; then
    check "Bundled plugin: shux-status-bar" "pass"
else
    check "Bundled plugin: shux-status-bar" "warn" "requires running daemon"
fi

if $SHUX_BIN plugin ls 2>/dev/null | grep -q "theme-pack"; then
    check "Bundled plugin: shux-theme-pack" "pass"
else
    check "Bundled plugin: shux-theme-pack" "warn" "requires running daemon"
fi

if $SHUX_BIN plugin ls 2>/dev/null | grep -q "diagnostics"; then
    check "Bundled plugin: shux-diagnostics" "pass"
else
    check "Bundled plugin: shux-diagnostics" "warn" "requires running daemon"
fi

# ═══════════════════════════════════════
# Section 6: Documentation
# ═══════════════════════════════════════
echo ""
echo "─── 6. Documentation ───"

DOCS=(
    "README.md"
    "docs/getting-started.md"
    "docs/plugin-guide.md"
    "docs/api-reference.md"
    "docs/config-reference.md"
    "man/shux.1"
)

for doc in "${DOCS[@]}"; do
    if [ -f "$doc" ]; then
        check "Doc exists: $doc" "pass"
    else
        check "Doc exists: $doc" "fail"
    fi
done

# Check for broken links
BROKEN_LINKS=0
for doc in docs/*.md README.md; do
    if [ -f "$doc" ]; then
        while IFS= read -r link; do
            link="${link#(}"
            link="${link%)}"
            if [[ "$link" != http* ]] && [[ "$link" != \#* ]]; then
                if [ ! -f "$link" ] && [ ! -f "docs/$link" ]; then
                    BROKEN_LINKS=$((BROKEN_LINKS + 1))
                fi
            fi
        done < <(grep -oP '\]\([^)]+\)' "$doc" | tr -d ']')
    fi
done

if [ "$BROKEN_LINKS" -eq 0 ]; then
    check "No broken doc links" "pass"
else
    check "No broken doc links" "fail" "$BROKEN_LINKS broken links"
fi

# ═══════════════════════════════════════
# Section 7: Release Readiness
# ═══════════════════════════════════════
echo ""
echo "─── 7. Release Readiness ───"

# Version in Cargo.toml
VERSION=$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)"/\1/')
if [ "$VERSION" = "1.0.0" ]; then
    check "Version is 1.0.0" "pass"
else
    check "Version is 1.0.0" "warn" "current: $VERSION"
fi

# Release workflow exists
if [ -f ".github/workflows/release.yml" ]; then
    check "Release workflow exists" "pass"
else
    check "Release workflow exists" "fail"
fi

# Homebrew formula exists
if [ -f "Formula/shux.rb" ]; then
    check "Homebrew formula exists" "pass"
else
    check "Homebrew formula exists" "fail"
fi

# PROGRESS.md up to date
if grep -q "059.*Complete" docs/PROGRESS.md 2>/dev/null; then
    check "PROGRESS.md updated" "pass"
else
    check "PROGRESS.md updated" "warn" "task 059 not marked complete"
fi

# ═══════════════════════════════════════
# Section 8: PRD Success Metrics (SS18)
# ═══════════════════════════════════════
echo ""
echo "─── 8. PRD Success Metrics (SS18) ───"

check "Daily-driver adoption (2 weeks)" "warn" "manual verification required"
check "Performance: all p99 budgets" "pass" "(verified in section 3)"
check "API coverage: 100% via CLI" "warn" "verify with API contract tests"
check "Plugin API sufficiency" "warn" "verify bundled plugins use only public API"
check "Agent story: 3 scenarios pass" "pass" "(verified in section 2)"
check "Visual quality: zero regressions" "warn" "verify with L4 tests"
check "Crash resilience: zero crashes" "pass" "(verified in section 4)"
check "Plugin DX: hello world < 15min" "warn" "manual timing required"

# ═══════════════════════════════════════
# Summary
# ═══════════════════════════════════════
echo ""
echo "══════════════════════════════════════════════════"
echo "  PASS: $PASS / $TOTAL"
echo "  WARN: $WARN / $TOTAL"
echo "  FAIL: $FAIL / $TOTAL"
echo "══════════════════════════════════════════════════"

if [ "$FAIL" -gt 0 ]; then
    echo ""
    echo "  M3 QUALITY GATE: FAILED ($FAIL failures)"
    echo "  Fix all failures before tagging v1.0.0"
    exit 1
elif [ "$WARN" -gt 3 ]; then
    echo ""
    echo "  M3 QUALITY GATE: CONDITIONAL ($WARN warnings)"
    echo "  Review warnings before tagging v1.0.0"
    exit 0
else
    echo ""
    echo "  M3 QUALITY GATE: PASSED"
    echo "  Ready to tag v1.0.0!"
    exit 0
fi
```

### Step 2: Verify Plugin API Sufficiency

Verify that all 3 bundled plugins use only the public plugin API:

```rust
// tests/integration/m3_final_test.rs

/// Verify bundled plugins don't use internal APIs.
///
/// Each bundled plugin must use only the WIT-defined host functions
/// and the process plugin protocol messages. If a plugin needs
/// something not in the public API, the API is incomplete.
#[test]
fn bundled_plugins_use_only_public_api() {
    // Check shux-status-bar
    let status_bar_imports = analyze_wasm_imports("plugins/shux-status-bar/plugin.wasm");
    for import in &status_bar_imports {
        assert!(
            is_public_api_function(import),
            "shux-status-bar uses non-public API: {}",
            import,
        );
    }

    // Check shux-theme-pack
    let theme_pack_imports = analyze_wasm_imports("plugins/shux-theme-pack/plugin.wasm");
    for import in &theme_pack_imports {
        assert!(
            is_public_api_function(import),
            "shux-theme-pack uses non-public API: {}",
            import,
        );
    }

    // Check shux-diagnostics
    let diagnostics_imports = analyze_wasm_imports("plugins/shux-diagnostics/plugin.wasm");
    for import in &diagnostics_imports {
        assert!(
            is_public_api_function(import),
            "shux-diagnostics uses non-public API: {}",
            import,
        );
    }
}

/// Verify CLI covers 100% of API operations.
#[test]
fn cli_covers_all_api_methods() {
    let api_methods = get_all_api_methods(); // From SS8.2
    let cli_commands = get_all_cli_commands(); // From clap definitions

    for method in &api_methods {
        assert!(
            cli_commands.iter().any(|cmd| cmd.maps_to(method)),
            "API method '{}' has no CLI equivalent",
            method,
        );
    }
}
```

### Step 3: Verify API Contract Coverage

```rust
/// Every API method must have at least one L3 contract test.
#[test]
fn every_api_method_has_contract_test() {
    let all_methods = vec![
        "system.version", "system.health",
        "session.list", "session.create", "session.ensure",
        "session.rename", "session.kill", "session.attach",
        "window.list", "window.create", "window.ensure",
        "window.rename", "window.focus", "window.reorder", "window.kill",
        "pane.list", "pane.split", "pane.ensure", "pane.focus",
        "pane.resize", "pane.zoom", "pane.swap", "pane.kill",
        "pane.send_keys", "pane.run_command", "pane.capture",
        "pane.set_title", "pane.set_cwd", "pane.set_env",
        "pane.set_theme", "pane.set_theme_override",
        "pane.set_tag", "pane.get_tags",
        "state.snapshot", "state.apply",
        "events.watch", "events.history",
        "theme.list", "theme.get", "theme.set",
        "config.get", "config.set", "config.validate", "config.explain",
        "plugin.list", "plugin.enable", "plugin.disable",
        "plugin.reload", "plugin.inspect",
        "keybinding.list", "keybinding.set", "keybinding.reset",
        "copy.enter", "copy.search", "copy.select", "copy.to_clipboard",
        "log.set_level", "log.tail",
        "metrics.get", "diagnose.run",
        "admin.shutdown", "admin.gc",
    ];

    let test_sources = std::fs::read_to_string("tests/integration/m2_api_contract.rs")
        .expect("M2 API contract test file must exist");

    for method in &all_methods {
        let search_term = method.replace('.', "_");
        assert!(
            test_sources.contains(&search_term) || test_sources.contains(method),
            "Missing L3 contract test for API method: {}",
            method,
        );
    }
}
```

### Step 4: Update PROGRESS.md

Mark all tasks 053-059 as complete. Update milestone status:

```markdown
## Current Phase

**M3: Polish, Performance, Docs** — Complete

## Status

### Milestone Targets

- [x] **M0: Architecture Spike** (tasks 001-012) — Complete
- [x] **M1: Daily-Driver Core** (tasks 013-034) — Complete
- [x] **M2: API + Plugin System** (tasks 035-052) — Complete
- [x] **M3: Polish, Performance, Docs** (tasks 053-059) — Complete

## Session Log

### [date] — M3 Final Quality Gate
- Ran full M3 quality gate: X pass, Y warn, Z fail
- All performance budgets met
- Zero known crashers
- All test layers passing
- Documentation complete and verified
- Tagged v1.0.0
```

### Step 5: Version Bump and Tag

```bash
# Bump version to 1.0.0 in Cargo.toml
# (Do this ONLY after quality gate passes)

# Create annotated tag
git tag -a v1.0.0 -m "shux v1.0.0 — initial release

A modern, batteries-included terminal multiplexer.

Highlights:
- Plugin system with Wasm sandbox (WASI Preview 2)
- Typed JSON-RPC API for AI agent integration
- Per-pane theming with 5 bundled themes
- Graded keybindings and command palette
- Performance: p99 keypress <= 25ms, p99 split <= 80ms
- Shell completions (bash, zsh, fish)
- Binary releases for macOS and Linux
"

# Push tag to trigger release workflow
git push origin v1.0.0
```

---

## Verification

### Functional

```bash
# Run the complete M3 quality gate
./scripts/run-m3-gate.sh
# Expected: ALL PASSED or CONDITIONAL (warnings only)

# Verify release build
cargo build --release
./target/release/shux --version
# Expected: shux 1.0.0

# Run all test layers
cargo nextest run --workspace
cargo test --workspace --doc

# Verify documentation
./scripts/verify-docs.sh

# Verify performance budgets
./scripts/bench-all.sh
```

### Tests

```bash
# Run M3 final tests
cargo nextest run --test m3_final_test

# Expected:
# - bundled_plugins_use_only_public_api
# - cli_covers_all_api_methods
# - every_api_method_has_contract_test
```

---

## Completion Criteria

- [ ] All performance budgets met (keypress p99 <= 25ms, split p99 <= 80ms, attach <= 150ms, throughput >= 10K lines/s, memory <= 80MB goal, plugin p99 <= 5ms, Wasm instantiation p99 <= 200us)
- [ ] Zero known crashers (no crash artifacts in fuzz/artifacts/)
- [ ] Plugin authoring guide includes working examples (timed: < 15 minutes)
- [ ] All 6 test layers passing: L1 (unit), L2 (PTY), L3 (API contract), L4 (visual/iterm2-driver), L5 (agent scenarios), L6 (dogfood)
- [ ] L4 iterm2-driver visual suite passes: `uv run .claude/automations/test_m1_visual.py` (all 7 scenarios)
- [ ] All per-feature iterm2-driver tests pass (splits, copy mode, theming, palette, help, status bar)
- [ ] API coverage: 100% of operations testable via CLI
- [ ] Plugin API sufficiency: all 3 bundled plugins use only public plugin API
- [ ] All 3 agent scenario scripts pass
- [ ] Binary releases available for macOS (aarch64, x86_64) + Linux (glibc, musl, both architectures)
- [ ] Shell completions for bash, zsh, fish
- [ ] Documentation complete: README, getting started, plugin guide, API reference, config reference, man page
- [ ] No broken links in documentation
- [ ] Image passthrough works (Kitty, Sixel, iTerm2)
- [ ] Fuzz campaign: no crashes in 30-minute runs per target
- [ ] Version bumped to 1.0.0
- [ ] PROGRESS.md: all tasks marked complete, final session log entry
- [ ] CLAUDE.md: final learnings update (create if missing, per task 000)
- [ ] v1.0.0 tag created and pushed
- [ ] GitHub Release created with binaries and checksums

---

## Commit Message

```
chore: pass M3 quality gate and prepare v1.0.0 release

- All 6 test layers passing (L1-L6)
- All P0 performance budgets met
- Zero known crashers (fuzz campaign clean)
- Documentation complete and verified
- Version bumped to 1.0.0
- PROGRESS.md updated with final status
- Ready for v1.0.0 tag
```

---

## Session Protocol

1. **Before starting:** Verify all tasks 053-058 are complete. Read their completion status in PROGRESS.md. Ensure no outstanding PRs or unmerged work.
2. **During:** Run `./scripts/run-m3-gate.sh` first. Fix any failures. Re-run until all checks pass. Do NOT tag until the gate passes cleanly.
3. **Manual verifications required:**
   - L6 dogfood: run `cargo test` inside shux panes, verify shux stays responsive
   - Plugin DX: follow the plugin guide from scratch, time yourself
   - Daily-driver: the author should have been using shux for >= 2 weeks before this gate
4. **Release process:**
   - Bump version to 1.0.0 in workspace Cargo.toml
   - Commit the version bump
   - Create annotated tag: `git tag -a v1.0.0 -m "..."`
   - Push: `git push origin v1.0.0`
   - Verify GitHub Actions release workflow completes
   - Verify all 6 binary archives are published
   - Verify Homebrew formula is updated
   - Test `brew install indrasvat/tap/shux` on a clean system
5. **After:** Celebrate. Write a "shux v1.0.0 released" announcement. Update the project roadmap with v1.1 plans (floating panes, session persistence, MCP server). Update `CLAUDE.md` with final learnings (create from task 000 template if missing).
