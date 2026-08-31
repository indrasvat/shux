# Makefile for shux
# Modern, batteries-included terminal multiplexer

# ══════════════════════════════════════════════════════════════════════════════
# Variables
# ══════════════════════════════════════════════════════════════════════════════

# CI exports this for every job; without it `make check` is more permissive than
# CI and a warning-level defect passes locally then fails on push. `?=` so an
# explicit value still wins.
export RUSTFLAGS ?= -Dwarnings

BINARY_NAME := shux
VERSION := $(shell cargo metadata --format-version 1 --no-deps 2>/dev/null | grep -o '"version":"[^"]*"' | head -1 | cut -d'"' -f4 || echo "dev")
COMMIT := $(shell git rev-parse --short HEAD 2>/dev/null || echo "unknown")
RUST_VERSION := $(shell rustc --version 2>/dev/null | cut -d' ' -f2 || echo "unknown")
LIBGHOSTTY_SPIKE_ZIG_VERSION ?= 0.15.2
LIBGHOSTTY_SPIKE_ZIG_ARCH := $(shell uname -m | sed 's/^arm64$$/aarch64/')
LIBGHOSTTY_SPIKE_ZIG_OS := $(shell uname -s | tr '[:upper:]' '[:lower:]' | sed 's/^darwin$$/macos/')
LIBGHOSTTY_SPIKE_ZIG_PKG := zig-$(LIBGHOSTTY_SPIKE_ZIG_ARCH)-$(LIBGHOSTTY_SPIKE_ZIG_OS)-$(LIBGHOSTTY_SPIKE_ZIG_VERSION)
LIBGHOSTTY_SPIKE_ZIG_DIR := .local/tools/zig-$(LIBGHOSTTY_SPIKE_ZIG_VERSION)
LIBGHOSTTY_SPIKE_ZIG_URL := https://ziglang.org/download/$(LIBGHOSTTY_SPIKE_ZIG_VERSION)/$(LIBGHOSTTY_SPIKE_ZIG_PKG).tar.xz
LIBGHOSTTY_SPIKE_BREW_ZIG015_BIN ?= /opt/homebrew/opt/zig@0.15/bin

# Directories
COVERAGE_DIR := coverage

# Tools
#
# Every test target runs through nextest, so local runs and CI are the same
# command at the same concurrency — the divergence issue #130 is about. nextest
# runs each test in its own process, which is what makes the workspace safe to
# run in parallel at all: process-global state (env vars, cwd, signal
# disposition) stops being shared between tests. What stays genuinely shared —
# the machine's process table, the kernel PTY pool — is bounded by the test
# groups in .config/nextest.toml.
#
# `nextest-ready` installs the pre-built binary in seconds if it is missing, so
# a fresh clone or a cold container gets the fast path rather than the fallback.
# Test concurrency. The suite spends most of its time WAITING — on a daemon
# handshake, on a PTY, on a pane going quiet — not computing, so one thread per
# core (nextest's default) leaves most of the machine idle. Measured on a
# 4-core box: 69.5s at one-per-core, 22.0s at four-per-core, same 1,933 passes.
# The tests that genuinely cannot take that are held back by the groups in
# .config/nextest.toml, not by lowering this.
#
# Override with `make test NEXTEST_JOBS=8` on a machine that is busy with
# something else.
# `scripts/cpu-budget.sh`, NOT `getconf _NPROCESSORS_ONLN`. getconf counts the
# CPUs the MACHINE has; inside a container with a CPU quota that over-reports,
# and multiplying an over-report by four is how you get hundreds of test
# processes onto two usable CPUs — the exact starvation the scheduling here
# exists to prevent. The helper takes the smallest of affinity, cgroup v2
# `cpu.max` and cgroup v1 `cpu.cfs_quota_us`.
NEXTEST_CPUS := $(shell bash scripts/cpu-budget.sh 2>/dev/null || echo 4)
NEXTEST_JOBS ?= $(shell echo $$(( $(NEXTEST_CPUS) * 4 )))
# `--status-level fail`: print failures and the summary, not 2131 PASS lines.
# The pass list was 90% of pre-push output and buried the one line that matters.
# Override with `make test NEXTEST_STATUS=pass` when you want the roll call.
NEXTEST_STATUS ?= fail
NEXTEST_RUN = cargo nextest run -j $(NEXTEST_JOBS) --status-level $(NEXTEST_STATUS)
LEFTHOOK := $(shell command -v lefthook 2>/dev/null)

# Colors for output
COLOR_RESET := \033[0m
COLOR_BOLD := \033[1m
COLOR_GREEN := \033[32m
COLOR_YELLOW := \033[33m
COLOR_BLUE := \033[34m
COLOR_MAGENTA := \033[35m
COLOR_RED := \033[31m

# ══════════════════════════════════════════════════════════════════════════════
# Default target
# ══════════════════════════════════════════════════════════════════════════════

.DEFAULT_GOAL := help

# ══════════════════════════════════════════════════════════════════════════════
# Help
# ══════════════════════════════════════════════════════════════════════════════

.PHONY: help
help: ## Show this help message
	@echo ""
	@echo "$(COLOR_BOLD)shux - Modern terminal multiplexer$(COLOR_RESET)"
	@echo ""
	@echo "$(COLOR_BOLD)Usage:$(COLOR_RESET)"
	@echo "  make $(COLOR_GREEN)<target>$(COLOR_RESET)"
	@echo ""
	@echo "$(COLOR_BOLD)Build:$(COLOR_RESET)"
	@awk 'BEGIN {FS = ":.*##"} /^(build|release|install)[a-zA-Z_-]*:.*?##/ { printf "  $(COLOR_GREEN)%-20s$(COLOR_RESET) %s\n", $$1, $$2 }' $(MAKEFILE_LIST)
	@echo ""
	@echo "$(COLOR_BOLD)Test:$(COLOR_RESET)"
	@awk 'BEGIN {FS = ":.*##"} /^(test|bench)[a-zA-Z_-]*:.*?##/ { printf "  $(COLOR_GREEN)%-20s$(COLOR_RESET) %s\n", $$1, $$2 }' $(MAKEFILE_LIST)
	@echo ""
	@echo "$(COLOR_BOLD)Code Quality:$(COLOR_RESET)"
	@awk 'BEGIN {FS = ":.*##"} /^(clippy|lint|fmt|check|ci|deny|fuzz|shellcheck)[a-zA-Z_-]*:.*?##/ { printf "  $(COLOR_GREEN)%-20s$(COLOR_RESET) %s\n", $$1, $$2 }' $(MAKEFILE_LIST)
	@echo ""
	@echo "$(COLOR_BOLD)Tooling:$(COLOR_RESET)"
	@awk 'BEGIN {FS = ":.*##"} /^(setup|hooks|doc|clean|version|info|reap)[a-zA-Z_-]*:.*?##/ { printf "  $(COLOR_GREEN)%-20s$(COLOR_RESET) %s\n", $$1, $$2 }' $(MAKEFILE_LIST)
	@echo ""
	@echo "$(COLOR_BOLD)Spikes:$(COLOR_RESET)"
	@awk 'BEGIN {FS = ":.*##"} /^spike[a-zA-Z0-9_-]*:.*?##/ { printf "  $(COLOR_GREEN)%-20s$(COLOR_RESET) %s\n", $$1, $$2 }' $(MAKEFILE_LIST)
	@echo ""

# ══════════════════════════════════════════════════════════════════════════════
# Build
# ══════════════════════════════════════════════════════════════════════════════

.PHONY: build
build: ## Build all crates (debug)
	@echo "$(COLOR_BLUE)▶ Building $(BINARY_NAME) (debug)...$(COLOR_RESET)"
	@cargo build --workspace
	@echo "$(COLOR_GREEN)✓ Built target/debug/$(BINARY_NAME)$(COLOR_RESET)"

.PHONY: release
release: ## Build optimized binary
	@echo "$(COLOR_BLUE)▶ Building $(BINARY_NAME) (release)...$(COLOR_RESET)"
	@cargo build --release
	@echo "$(COLOR_GREEN)✓ Built target/release/$(BINARY_NAME)$(COLOR_RESET)"

.PHONY: install
install: release ## Install to ~/.local/bin
	@echo "$(COLOR_BLUE)▶ Installing $(BINARY_NAME)...$(COLOR_RESET)"
	@install -d ~/.local/bin
	@install -m 755 target/release/$(BINARY_NAME) ~/.local/bin/$(BINARY_NAME)
	@echo "$(COLOR_GREEN)✓ Installed to ~/.local/bin/$(BINARY_NAME)$(COLOR_RESET)"

.PHONY: nextest-ready
nextest-ready:
	@bash scripts/ensure-nextest.sh

.PHONY: setup-nextest
setup-nextest: ## Install cargo-nextest (pre-built binary; seconds, not minutes)
	@bash scripts/ensure-nextest.sh

.PHONY: setup-bench
setup-bench: ## Install hyperfine, the benchmark harness `make bench-test-suite` uses
	@bash scripts/ensure-hyperfine.sh

.PHONY: setup-deny
setup-deny: ## Install cargo-deny (pre-built binary; seconds, not minutes)
	@bash scripts/ensure-deny.sh

.PHONY: check-ci-parity
check-ci-parity: nextest-ready ## Run the cargo-output parsers under CI's environment (colour on)
	@bash scripts/check-ci-parity.sh

.PHONY: check-darwin
check-darwin: ## Type-check the workspace for macOS — catches cfg-gated APIs a Linux build never sees
	@# Needs the -Dwarnings above: dead cfg-gated items are warnings, so without
	@# it this printed the defect and reported success.
	@echo "$(COLOR_BLUE)▶ Cross-checking aarch64-apple-darwin...$(COLOR_RESET)"
	@rustup target list --installed | grep -qx aarch64-apple-darwin \
	  || rustup target add aarch64-apple-darwin
	@cargo check --workspace --all-targets --target aarch64-apple-darwin --color never
	@echo "$(COLOR_GREEN)✓ macOS type-check passed$(COLOR_RESET)"

.PHONY: check-test-groups
check-test-groups: nextest-ready ## Assert every nextest test-group still matches the tests it bounds
	@bash scripts/check-test-groups.sh

.PHONY: check-test-inventory
check-test-inventory: nextest-ready ## Assert no test stopped being compiled since BASE_REF (default origin/main)
	@bash scripts/check-test-inventory.sh

.PHONY: bench-test-suite
bench-test-suite: nextest-ready setup-bench ## hyperfine A/B: legacy serial runner vs the parallel one
	@PATH="$${CARGO_HOME:-$$HOME/.cargo}/bin:$$PATH" bash .shux/scripts/bench_test_suite.sh

.PHONY: bench-edit-loop
bench-edit-loop: nextest-ready ## Time the real loop: change code, run tests, repeat
	@PATH="$${CARGO_HOME:-$$HOME/.cargo}/bin:$$PATH" bash .shux/scripts/bench_edit_loop.sh

.PHONY: test-fallback
test-fallback: ## Legacy serial runner (no nextest). Kept for hosts where nextest cannot run.
	@echo "$(COLOR_YELLOW)▶ Legacy serial runner — this is the slow path, and it is NOT what CI runs.$(COLOR_RESET)"
	@.shux/scripts/no_leak_guard.sh bash scripts/run-cargo-test.sh --workspace -- --test-threads=1

.PHONY: install-tools
install-tools: ## Install dev dependencies (nextest, llvm-cov, deny, hyperfine, lefthook)
	@echo "$(COLOR_BLUE)▶ Installing dev tools...$(COLOR_RESET)"
	@bash scripts/ensure-nextest.sh
	cargo install cargo-llvm-cov --locked
	@bash scripts/ensure-deny.sh
	@bash scripts/ensure-hyperfine.sh
	cargo install lefthook --locked || npm i -g lefthook
	@echo "$(COLOR_GREEN)✓ Dev tools installed$(COLOR_RESET)"

# ══════════════════════════════════════════════════════════════════════════════
# Test
# ══════════════════════════════════════════════════════════════════════════════

.PHONY: test
test: nextest-ready ## Run the whole workspace suite (same command CI runs)
	@echo "$(COLOR_BLUE)▶ Running tests...$(COLOR_RESET)"
	@.shux/scripts/no_leak_guard.sh $(NEXTEST_RUN) --workspace
	@echo "$(COLOR_GREEN)✓ Tests passed$(COLOR_RESET)"

.PHONY: test-verbose
test-verbose: nextest-ready ## Run tests with output visible
	@echo "$(COLOR_BLUE)▶ Running tests (verbose)...$(COLOR_RESET)"
	@.shux/scripts/no_leak_guard.sh $(NEXTEST_RUN) --workspace --no-capture

.PHONY: test-lib
test-lib: nextest-ready ## Run library tests only
	@echo "$(COLOR_BLUE)▶ Running library tests...$(COLOR_RESET)"
	@$(NEXTEST_RUN) --workspace --lib
	@echo "$(COLOR_GREEN)✓ Library tests passed$(COLOR_RESET)"

.PHONY: test-vt
test-vt: nextest-ready ## Run focused virtual terminal tests; optionally pass FILTER=<test-name>
	@echo "$(COLOR_BLUE)▶ Running virtual terminal tests...$(COLOR_RESET)"
	@$(NEXTEST_RUN) -p shux-vt --lib $(FILTER)
	@echo "$(COLOR_GREEN)✓ Virtual terminal tests passed$(COLOR_RESET)"

.PHONY: test-window-snapshot
test-window-snapshot: nextest-ready ## Run the window-snapshot oversize cross-path integration test; optionally pass FILTER=<test-name>
	@echo "$(COLOR_BLUE)▶ Running window snapshot oversize integration tests...$(COLOR_RESET)"
	@.shux/scripts/no_leak_guard.sh $(NEXTEST_RUN) -p shux --test window_snapshot_oversize $(FILTER)
	@echo "$(COLOR_GREEN)✓ Window snapshot oversize tests passed$(COLOR_RESET)"

.PHONY: test-pane-io
test-pane-io: nextest-ready ## Run pane I/O integration tests; optionally pass FILTER=<test-name>
	@echo "$(COLOR_BLUE)▶ Running pane I/O integration tests...$(COLOR_RESET)"
	@.shux/scripts/no_leak_guard.sh $(NEXTEST_RUN) -p shux --test pane_io_integration $(FILTER)
	@echo "$(COLOR_GREEN)✓ Pane I/O integration tests passed$(COLOR_RESET)"

.PHONY: test-pane-command
test-pane-command: nextest-ready ## Run pane command / exited-pane e2e tests; optionally pass FILTER=<test-name>
	@echo "$(COLOR_BLUE)▶ Running pane command e2e tests...$(COLOR_RESET)"
	@.shux/scripts/no_leak_guard.sh $(NEXTEST_RUN) -p shux --test pane_command_e2e $(FILTER)
	@echo "$(COLOR_GREEN)✓ Pane command e2e tests passed$(COLOR_RESET)"

.PHONY: test-window-snapshot-border-style
test-window-snapshot-border-style: nextest-ready ## Run the `window snapshot` border-style cross-path test; optionally pass FILTER=<test-name>
	@echo "$(COLOR_BLUE)▶ Running window snapshot border-style tests...$(COLOR_RESET)"
	@.shux/scripts/no_leak_guard.sh $(NEXTEST_RUN) -p shux --test window_snapshot_border_style $(FILTER)
	@echo "$(COLOR_GREEN)✓ Window snapshot border-style tests passed$(COLOR_RESET)"

.PHONY: test-pane-list-columns
test-pane-list-columns: nextest-ready ## Run `pane list` column/cross-path tests; optionally pass FILTER=<test-name>
	@echo "$(COLOR_BLUE)▶ Running pane list column tests...$(COLOR_RESET)"
	@.shux/scripts/no_leak_guard.sh $(NEXTEST_RUN) -p shux --test pane_list_columns $(FILTER)
	@echo "$(COLOR_GREEN)✓ Pane list column tests passed$(COLOR_RESET)"

.PHONY: test-exit-capture-evidence
test-exit-capture-evidence: build ## Issue #162 exit-then-capture A/B; pass BASE_BIN=<macOS-arm binary> to also record the pre-fix arm
	@echo "$(COLOR_BLUE)▶ issue-162 exit-then-capture (fixed binary)...$(COLOR_RESET)"
	@SHUX_BIN=$(CURDIR)/target/debug/shux LABEL=after .shux/scripts/issue_162_evidence.sh
	@if [ -n "$(BASE_BIN)" ]; then \
	  echo "$(COLOR_BLUE)▶ issue-162 exit-then-capture (pre-fix binary)...$(COLOR_RESET)"; \
	  SHUX_BIN=$(BASE_BIN) LABEL=before EXPECT_DEFECT=1 .shux/scripts/issue_162_evidence.sh; \
	fi
	@echo "$(COLOR_GREEN)✓ issue-162 evidence recorded under .shux/out/issue-162/$(COLOR_RESET)"

.PHONY: test-id-refs-evidence
test-id-refs-evidence: build ## Issue #120 A/B round trip against the real binary; pass BASE_BIN=<path> to also record the pre-fix arm
	@echo "$(COLOR_BLUE)▶ issue-120 round trip (fixed binary)...$(COLOR_RESET)"
	@SHUX_BIN=$(CURDIR)/target/debug/shux LABEL=after .shux/scripts/issue_120_evidence.sh
	@if [ -n "$(BASE_BIN)" ]; then \
	  echo "$(COLOR_BLUE)▶ issue-120 round trip (pre-fix binary)...$(COLOR_RESET)"; \
	  SHUX_BIN=$(BASE_BIN) LABEL=before EXPECT_DEFECT=1 .shux/scripts/issue_120_evidence.sh; \
	fi
	@echo "$(COLOR_GREEN)✓ issue-120 evidence recorded under .shux/out/issue-120/$(COLOR_RESET)"

.PHONY: test-id-refs
test-id-refs: nextest-ready ## Run entity-id reference (short-id / prefix) tests end to end; optionally pass FILTER=<test-name>
	@echo "$(COLOR_BLUE)▶ Running entity-id reference tests...$(COLOR_RESET)"
	@$(NEXTEST_RUN) -p shux-core --lib idref $(FILTER)
	@.shux/scripts/no_leak_guard.sh $(NEXTEST_RUN) -p shux --test id_prefix_resolution $(FILTER)
	@echo "$(COLOR_GREEN)✓ Entity-id reference tests passed$(COLOR_RESET)"

.PHONY: test-rpc
test-rpc: nextest-ready ## Run shux-rpc crate tests (codec/router/server/attach); optionally pass FILTER=<test-name>
	@echo "$(COLOR_BLUE)▶ Running shux-rpc tests...$(COLOR_RESET)"
	@$(NEXTEST_RUN) -p shux-rpc $(FILTER)
	@echo "$(COLOR_GREEN)✓ shux-rpc tests passed$(COLOR_RESET)"

.PHONY: test-cli-unit
test-cli-unit: nextest-ready ## Run shux CLI unit tests (bin target; mock-stream, no daemons); optionally pass FILTER=<test-name>
	@echo "$(COLOR_BLUE)▶ Running shux CLI unit tests...$(COLOR_RESET)"
	@$(NEXTEST_RUN) -p shux --bin shux $(FILTER)
	@echo "$(COLOR_GREEN)✓ shux CLI unit tests passed$(COLOR_RESET)"

.PHONY: test-mutation-suite
test-mutation-suite: ## One suite for the mutation batteries, in libtest format. CRATE=<pkg> TARGET=<--lib|--bin shux> FILTER=<path::to::tests>
	@# Deliberately `cargo test`, not nextest: the batteries parse libtest's
	@# indented `failures:` block, which nextest does not emit. `--color never`
	@# is pinned HERE so no caller can forget it — CI exports
	@# CARGO_TERM_COLOR=always and cargo only colours through a TTY, so the
	@# coloured path is the one local runs never see (`make check-ci-parity`).
	@cargo test --color never -p $(CRATE) $(TARGET) -- $(FILTER)

.PHONY: test-plugin-dx
test-plugin-dx: nextest-ready ## Run focused plugin DX CLI/integration tests; optionally pass FILTER=<test-name>
	@echo "$(COLOR_BLUE)▶ Running plugin DX tests...$(COLOR_RESET)"
	@$(NEXTEST_RUN) -p shux --bin shux $(FILTER)
	@.shux/scripts/no_leak_guard.sh $(NEXTEST_RUN) -p shux --test cli_integration $(FILTER)
	@echo "$(COLOR_GREEN)✓ Plugin DX tests passed$(COLOR_RESET)"

.PHONY: test-sightline
test-sightline: release ## Run focused Sightline plugin/package checks
	@echo "$(COLOR_BLUE)▶ Running Sightline checks...$(COLOR_RESET)"
	@.shux/scripts/no_leak_guard.sh bash .shux/scripts/sightline_check.sh
	@echo "$(COLOR_GREEN)✓ Sightline checks passed$(COLOR_RESET)"

.PHONY: test-vt-corpus-unit
test-vt-corpus-unit: nextest-ready ## Run VT corpus replay unit/integration tests
	@echo "$(COLOR_BLUE)▶ Running VT corpus replay tests...$(COLOR_RESET)"
	@$(NEXTEST_RUN) -p shux-vt --test vt_corpus_replay
	@echo "$(COLOR_GREEN)✓ VT corpus replay tests passed$(COLOR_RESET)"

.PHONY: test-vt-cow-aliasing
test-vt-cow-aliasing: nextest-ready ## Run copy-on-write / sync-freeze aliasing tests; optionally pass FILTER=<test-name>
	@echo "$(COLOR_BLUE)▶ Running VT copy-on-write aliasing tests...$(COLOR_RESET)"
	@$(NEXTEST_RUN) -p shux-vt --test cow_aliasing_adversarial $(FILTER)
	@echo "$(COLOR_GREEN)✓ VT copy-on-write aliasing tests passed$(COLOR_RESET)"

.PHONY: test-vt-wide-invariants
test-vt-wide-invariants: nextest-ready ## Run wide-cell invariant property tests
	@echo "$(COLOR_BLUE)▶ Running VT wide-cell invariant tests...$(COLOR_RESET)"
	@$(NEXTEST_RUN) -p shux-vt --test wide_invariants
	@echo "$(COLOR_GREEN)✓ VT wide-cell invariant tests passed$(COLOR_RESET)"

.PHONY: test-vt-corpus
test-vt-corpus: test-vt-corpus-unit ## Replay committed VT corpus fixtures and verify text/PNG goldens
	@echo "$(COLOR_BLUE)▶ Running VT corpus regression harness...$(COLOR_RESET)"
	@.shux/scripts/vt_corpus_check.sh
	@echo "$(COLOR_GREEN)✓ VT corpus regression harness passed$(COLOR_RESET)"

.PHONY: test-vt-resize-reflow
test-vt-resize-reflow: release ## Drive shux pane resize reflow automation and exact PNG return check
	@echo "$(COLOR_BLUE)▶ Running VT resize reflow automation...$(COLOR_RESET)"
	@.shux/scripts/no_leak_guard.sh .shux/scripts/resize_reflow_check.sh
	@echo "$(COLOR_GREEN)✓ VT resize reflow automation passed$(COLOR_RESET)"

.PHONY: test-vt-wide-visual
test-vt-wide-visual: release ## Drive shux wide-cell invariant visual/pixel automation
	@echo "$(COLOR_BLUE)▶ Running VT wide-cell visual automation...$(COLOR_RESET)"
	@.shux/scripts/no_leak_guard.sh .shux/scripts/wide_invariants_check.sh
	@echo "$(COLOR_GREEN)✓ VT wide-cell visual automation passed$(COLOR_RESET)"

.PHONY: test-vt-grapheme
test-vt-grapheme: release ## Drive shux grapheme storage visual/pixel automation
	@echo "$(COLOR_BLUE)▶ Running VT grapheme storage automation...$(COLOR_RESET)"
	@.shux/scripts/no_leak_guard.sh .shux/scripts/grapheme_check.sh
	@echo "$(COLOR_GREEN)✓ VT grapheme storage automation passed$(COLOR_RESET)"

# Deliberately NOT in `make check` or `make ci`: this needs an X server and a GUI
# terminal, and it costs minutes. Image work and emit-path changes invoke it.
# docs/agents/visual-testing.md says what it can and cannot see.
.PHONY: test-gui-terminal-selftest
test-gui-terminal-selftest: release ## Prove the GUI-terminal rig can FAIL (kitty + Xvfb; ~1 min)
	@echo "$(COLOR_BLUE)▶ Proving the GUI-terminal rig can fail...$(COLOR_RESET)"
	@.shux/scripts/no_leak_guard.sh bash scripts/check-gui-terminal-selftest.sh
	@echo "$(COLOR_GREEN)✓ GUI-terminal rig self-test passed$(COLOR_RESET)"

.PHONY: test-gui-terminal
test-gui-terminal: test-gui-terminal-selftest ## Photograph shux in a real GUI terminal (kitty + Xvfb)
	@echo "$(COLOR_BLUE)▶ Running the GUI-terminal rig...$(COLOR_RESET)"
	@.shux/scripts/no_leak_guard.sh bash .shux/scripts/gui_terminal_check.sh --scenario plain
	@echo "$(COLOR_GREEN)✓ GUI-terminal rig passed$(COLOR_RESET)"

.PHONY: test-vt-dec-special-graphics
test-vt-dec-special-graphics: release ## Drive DEC special graphics visual/pixel automation
	@echo "$(COLOR_BLUE)▶ Running VT DEC special graphics automation...$(COLOR_RESET)"
	@.shux/scripts/no_leak_guard.sh .shux/scripts/dec_special_graphics_check.sh
	@echo "$(COLOR_GREEN)✓ VT DEC special graphics automation passed$(COLOR_RESET)"

.PHONY: test-vt-tab-stops
test-vt-tab-stops: release ## Drive mutable tab-stop visual/pixel automation
	@echo "$(COLOR_BLUE)▶ Running VT tab-stop automation...$(COLOR_RESET)"
	@.shux/scripts/no_leak_guard.sh .shux/scripts/tab_stops_check.sh
	@echo "$(COLOR_GREEN)✓ VT tab-stop automation passed$(COLOR_RESET)"

.PHONY: test-vt-origin-mode
test-vt-origin-mode: release ## Drive origin-mode scroll-region visual/pixel automation
	@echo "$(COLOR_BLUE)▶ Running VT origin-mode automation...$(COLOR_RESET)"
	@.shux/scripts/no_leak_guard.sh .shux/scripts/origin_mode_check.sh
	@echo "$(COLOR_GREEN)✓ VT origin-mode automation passed$(COLOR_RESET)"

.PHONY: test-vt-dirty-regions
test-vt-dirty-regions: release ## Drive dirty-region tracking evidence, performance, and pixel automation
	@echo "$(COLOR_BLUE)▶ Running VT dirty-region automation...$(COLOR_RESET)"
	@.shux/scripts/no_leak_guard.sh .shux/scripts/dirty_region_check.sh
	@echo "$(COLOR_GREEN)✓ VT dirty-region automation passed$(COLOR_RESET)"

.PHONY: reap
reap: ## Stop shux daemons from this checkout left behind by an interrupted run
	@.shux/scripts/reap_daemons.sh

.PHONY: test-shux-leak-guard
test-shux-leak-guard: release ## Verify shux automation leak guard catches and kills orphan daemons
	@echo "$(COLOR_BLUE)▶ Running shux leak-guard self-test...$(COLOR_RESET)"
	@.shux/scripts/leak_guard_selftest.sh
	@echo "$(COLOR_GREEN)✓ Shux leak-guard self-test passed$(COLOR_RESET)"

.PHONY: test-agent-review-guard
test-agent-review-guard: ## Verify external reviewer guard kills timed-out process trees
	@echo "$(COLOR_BLUE)▶ Running agent review guard self-test...$(COLOR_RESET)"
	@.shux/scripts/agent_review_guard_selftest.sh
	@echo "$(COLOR_GREEN)✓ Agent review guard self-test passed$(COLOR_RESET)"

.PHONY: test-vt-grapheme-performance
test-vt-grapheme-performance: ## Measure grapheme storage performance on ASCII VT path
	@echo "$(COLOR_BLUE)▶ Measuring VT grapheme storage performance...$(COLOR_RESET)"
	@.shux/scripts/grapheme_perf_check.sh
	@echo "$(COLOR_GREEN)✓ VT grapheme storage performance passed$(COLOR_RESET)"

.PHONY: promote-vt-corpus-baselines
promote-vt-corpus-baselines: ## Promote current VT corpus output into committed goldens for review
	@echo "$(COLOR_BLUE)▶ Promoting VT corpus baselines...$(COLOR_RESET)"
	@.shux/scripts/vt_corpus_promote.sh
	@echo "$(COLOR_GREEN)✓ VT corpus baselines promoted$(COLOR_RESET)"

.PHONY: record-vt-corpus
record-vt-corpus: release ## Record installed rich TUIs into .shux/out/073-vt-corpus/recordings
	@echo "$(COLOR_BLUE)▶ Recording VT corpus rich-TUI streams...$(COLOR_RESET)"
	@.shux/scripts/no_leak_guard.sh .shux/scripts/vt_corpus_record.sh
	@echo "$(COLOR_GREEN)✓ VT corpus rich-TUI recording pass completed$(COLOR_RESET)"

.PHONY: test-ui
test-ui: nextest-ready ## Run focused UI/rendering tests; optionally pass FILTER=<test-name>
	@echo "$(COLOR_BLUE)▶ Running UI/rendering tests...$(COLOR_RESET)"
	@$(NEXTEST_RUN) -p shux-ui --lib $(FILTER)
	@echo "$(COLOR_GREEN)✓ UI/rendering tests passed$(COLOR_RESET)"

.PHONY: test-attach-color
test-attach-color: release ## Verify attach preserves pane colors even when daemon inherits NO_COLOR
	@echo "$(COLOR_BLUE)▶ Running attach color regression check...$(COLOR_RESET)"
	@.shux/scripts/no_leak_guard.sh .shux/scripts/issue_69_attach_color_check.sh
	@echo "$(COLOR_GREEN)✓ Attach color regression check passed$(COLOR_RESET)"

.PHONY: test-lossless-record
test-lossless-record: release ## Verify lossless pane recording with real PTY/TUI tools
	@echo "$(COLOR_BLUE)▶ Running lossless pane record regression check...$(COLOR_RESET)"
	@.shux/scripts/no_leak_guard.sh .shux/scripts/issue_70_lossless_record_check.sh
	@echo "$(COLOR_GREEN)✓ Lossless pane record regression check passed$(COLOR_RESET)"

.PHONY: test-copy-mode
test-copy-mode: nextest-ready ## Run focused copy-mode and copy-overlay tests
	@echo "$(COLOR_BLUE)▶ Running copy-mode tests...$(COLOR_RESET)"
	@$(NEXTEST_RUN) -p shux-ui --lib copy_mode
	@$(NEXTEST_RUN) -p shux-ui --lib compositor::tests::test_compositor_does_not_churn_cursor_when_idle
	@$(NEXTEST_RUN) -p shux-ui --lib compositor::tests::test_compositor_moves_cursor_without_hide_show_when_only_cursor_changes
	@$(NEXTEST_RUN) -p shux --bin shux attach::tests::
	@echo "$(COLOR_GREEN)✓ Copy-mode tests passed$(COLOR_RESET)"

.PHONY: test-doc
test-doc: ## Run doc tests
	@echo "$(COLOR_BLUE)▶ Running doc tests...$(COLOR_RESET)"
	@cargo test --workspace --doc
	@echo "$(COLOR_GREEN)✓ Doc tests passed$(COLOR_RESET)"

.PHONY: test-coverage
test-coverage: nextest-ready ## Run tests with coverage report
	@echo "$(COLOR_BLUE)▶ Running tests with coverage...$(COLOR_RESET)"
	@mkdir -p $(COVERAGE_DIR)
	@.shux/scripts/no_leak_guard.sh cargo llvm-cov nextest --workspace --lcov --output-path $(COVERAGE_DIR)/lcov.info
	@echo "$(COLOR_GREEN)✓ Coverage report: $(COVERAGE_DIR)/lcov.info$(COLOR_RESET)"

.PHONY: bench
bench: ## Show which benchmarks actually exist (there are no cargo #[bench] targets)
	@echo "$(COLOR_YELLOW)No [[bench]] targets, no benches/ directory, no criterion dependency.$(COLOR_RESET)"
	@echo ""
	@echo "This target used to run 'cargo bench --workspace', which compiled the"
	@echo "entire workspace in the bench profile and then ran zero benchmarks."
	@echo ""
	@echo "The real ones:"
	@echo "  make bench-test-suite   hyperfine A/B of the test suite (issue #130)"
	@echo "  make bench-lens-gate    lens capture/compare/render throughput"
	@echo "  make bench-baseline     M0 binary-size + suite baseline"

.PHONY: bench-baseline
bench-baseline: ## Record M0 performance baseline
	@./scripts/bench-baseline.sh

.PHONY: dogfood-human-copy
dogfood-human-copy: build ## Run copy-mode human dogfood regression
	@.shux/scripts/no_leak_guard.sh bash .shux/scripts/human_copy_mode_check.sh

.PHONY: spike-libghostty-build
spike-libghostty-build: ## Build the isolated libghostty-vt spike using Homebrew zig@0.15
	@echo "$(COLOR_BLUE)▶ Building libghostty-vt spike...$(COLOR_RESET)"
	@test -x "$(LIBGHOSTTY_SPIKE_BREW_ZIG015_BIN)/zig" || { echo "$(COLOR_RED)zig@0.15 not found. Run: brew install zig@0.15$(COLOR_RESET)"; exit 1; }
	@PATH="$(LIBGHOSTTY_SPIKE_BREW_ZIG015_BIN):$$PATH" CARGO_NET_GIT_FETCH_WITH_CLI=true cargo build --manifest-path spikes/libghostty-vt-eval/Cargo.toml
	@echo "$(COLOR_GREEN)✓ libghostty-vt spike built$(COLOR_RESET)"

.PHONY: spike-libghostty-test
spike-libghostty-test: ## Run the isolated libghostty-vt spike tests using Homebrew zig@0.15
	@echo "$(COLOR_BLUE)▶ Running libghostty-vt spike tests...$(COLOR_RESET)"
	@test -x "$(LIBGHOSTTY_SPIKE_BREW_ZIG015_BIN)/zig" || { echo "$(COLOR_RED)zig@0.15 not found. Run: brew install zig@0.15$(COLOR_RESET)"; exit 1; }
	@PATH="$(LIBGHOSTTY_SPIKE_BREW_ZIG015_BIN):$$PATH" CARGO_NET_GIT_FETCH_WITH_CLI=true cargo test --manifest-path spikes/libghostty-vt-eval/Cargo.toml -- --test-threads=1
	@echo "$(COLOR_GREEN)✓ libghostty-vt spike tests passed$(COLOR_RESET)"

.PHONY: spike-libghostty-compare
spike-libghostty-compare: ## Generate shux-vt vs libghostty-vt visual A/B comparisons
	@echo "$(COLOR_BLUE)▶ Generating libghostty-vt replacement comparisons...$(COLOR_RESET)"
	@test -x "$(LIBGHOSTTY_SPIKE_BREW_ZIG015_BIN)/zig" || { echo "$(COLOR_RED)zig@0.15 not found. Run: brew install zig@0.15$(COLOR_RESET)"; exit 1; }
	@PATH="$(LIBGHOSTTY_SPIKE_BREW_ZIG015_BIN):$$PATH" CARGO_NET_GIT_FETCH_WITH_CLI=true cargo run --manifest-path spikes/libghostty-vt-eval/Cargo.toml --bin compare --
	@echo "$(COLOR_GREEN)✓ libghostty-vt replacement comparisons generated$(COLOR_RESET)"

.PHONY: spike-libghostty-record-tuis
spike-libghostty-record-tuis: release ## Record installed rich TUIs as raw PTY fixtures for the libghostty-vt spike
	@echo "$(COLOR_BLUE)▶ Recording rich TUI PTY fixtures...$(COLOR_RESET)"
	@bash spikes/libghostty-vt-eval/scripts/record-rich-tuis.sh
	@echo "$(COLOR_GREEN)✓ rich TUI PTY fixtures recorded$(COLOR_RESET)"

.PHONY: spike-libghostty-compare-tuis
spike-libghostty-compare-tuis: ## Replay recorded rich TUI fixtures through shux-vt and libghostty-vt
	@echo "$(COLOR_BLUE)▶ Comparing recorded rich TUI PTY fixtures...$(COLOR_RESET)"
	@test -x "$(LIBGHOSTTY_SPIKE_BREW_ZIG015_BIN)/zig" || { echo "$(COLOR_RED)zig@0.15 not found. Run: brew install zig@0.15$(COLOR_RESET)"; exit 1; }
	@recordings=".shux/out/libghostty-vt-replacement/recordings/recordings.txt"; \
	if [ ! -s "$$recordings" ]; then echo "$(COLOR_RED)missing $$recordings; run make spike-libghostty-record-tuis$(COLOR_RESET)"; exit 1; fi; \
	args=""; \
	while IFS= read -r rec; do args="$$args --recording $$rec"; done < "$$recordings"; \
	PATH="$(LIBGHOSTTY_SPIKE_BREW_ZIG015_BIN):$$PATH" CARGO_NET_GIT_FETCH_WITH_CLI=true cargo run --manifest-path spikes/libghostty-vt-eval/Cargo.toml --bin compare -- $$args
	@echo "$(COLOR_GREEN)✓ recorded rich TUI comparisons generated$(COLOR_RESET)"

.PHONY: spike-libghostty-fmt
spike-libghostty-fmt: ## Format the isolated libghostty-vt spike crate
	@echo "$(COLOR_BLUE)▶ Formatting libghostty-vt spike...$(COLOR_RESET)"
	@cargo fmt --manifest-path spikes/libghostty-vt-eval/Cargo.toml
	@echo "$(COLOR_GREEN)✓ libghostty-vt spike formatted$(COLOR_RESET)"

.PHONY: spike-libghostty-build-zig015
spike-libghostty-build-zig015: ## Build the spike using Homebrew zig@0.15
	@echo "$(COLOR_BLUE)▶ Building libghostty-vt spike with Homebrew Zig $(LIBGHOSTTY_SPIKE_ZIG_VERSION)...$(COLOR_RESET)"
	@test -x "$(LIBGHOSTTY_SPIKE_BREW_ZIG015_BIN)/zig" || { echo "$(COLOR_RED)zig@0.15 not found. Run: brew install zig@0.15$(COLOR_RESET)"; exit 1; }
	@PATH="$(LIBGHOSTTY_SPIKE_BREW_ZIG015_BIN):$$PATH" CARGO_NET_GIT_FETCH_WITH_CLI=true cargo build --manifest-path spikes/libghostty-vt-eval/Cargo.toml
	@echo "$(COLOR_GREEN)✓ libghostty-vt spike built with Homebrew Zig $(LIBGHOSTTY_SPIKE_ZIG_VERSION)$(COLOR_RESET)"

.PHONY: spike-libghostty-test-zig015
spike-libghostty-test-zig015: ## Run spike tests using Homebrew zig@0.15
	@echo "$(COLOR_BLUE)▶ Running libghostty-vt spike tests with Homebrew Zig $(LIBGHOSTTY_SPIKE_ZIG_VERSION)...$(COLOR_RESET)"
	@test -x "$(LIBGHOSTTY_SPIKE_BREW_ZIG015_BIN)/zig" || { echo "$(COLOR_RED)zig@0.15 not found. Run: brew install zig@0.15$(COLOR_RESET)"; exit 1; }
	@PATH="$(LIBGHOSTTY_SPIKE_BREW_ZIG015_BIN):$$PATH" CARGO_NET_GIT_FETCH_WITH_CLI=true cargo test --manifest-path spikes/libghostty-vt-eval/Cargo.toml -- --test-threads=1
	@echo "$(COLOR_GREEN)✓ libghostty-vt spike tests passed with Homebrew Zig $(LIBGHOSTTY_SPIKE_ZIG_VERSION)$(COLOR_RESET)"

.PHONY: spike-libghostty-build-zig015-macos-target
spike-libghostty-build-zig015-macos-target: spike-libghostty-zig ## Build the spike with Zig 0.15.x and an explicit macOS Zig target
	@echo "$(COLOR_BLUE)▶ Building libghostty-vt spike with explicit Zig macOS target...$(COLOR_RESET)"
	@chmod +x spikes/libghostty-vt-eval/scripts/zig-macos-target-wrapper.sh
	@mkdir -p .local/tools/libghostty-zig-wrapper
	@ln -sf "$(CURDIR)/spikes/libghostty-vt-eval/scripts/zig-macos-target-wrapper.sh" .local/tools/libghostty-zig-wrapper/zig
	@PATH="$(CURDIR)/.local/tools/libghostty-zig-wrapper:$$PATH" SHUX_LIBGHOSTTY_REAL_ZIG="$(CURDIR)/$(LIBGHOSTTY_SPIKE_ZIG_DIR)/zig" CARGO_NET_GIT_FETCH_WITH_CLI=true cargo build --manifest-path spikes/libghostty-vt-eval/Cargo.toml
	@echo "$(COLOR_GREEN)✓ libghostty-vt spike built with explicit Zig macOS target$(COLOR_RESET)"

.PHONY: spike-libghostty-zig
spike-libghostty-zig: ## Install spike-local Zig 0.15.2 under .local/tools
	@if [ ! -x "$(LIBGHOSTTY_SPIKE_ZIG_DIR)/zig" ]; then \
		echo "$(COLOR_BLUE)▶ Installing Zig $(LIBGHOSTTY_SPIKE_ZIG_VERSION) for libghostty spike...$(COLOR_RESET)"; \
		mkdir -p .local/tools; \
		curl -fsSL "$(LIBGHOSTTY_SPIKE_ZIG_URL)" -o ".local/tools/$(LIBGHOSTTY_SPIKE_ZIG_PKG).tar.xz"; \
		tar -xJf ".local/tools/$(LIBGHOSTTY_SPIKE_ZIG_PKG).tar.xz" -C .local/tools; \
		rm -rf "$(LIBGHOSTTY_SPIKE_ZIG_DIR)"; \
		mv ".local/tools/$(LIBGHOSTTY_SPIKE_ZIG_PKG)" "$(LIBGHOSTTY_SPIKE_ZIG_DIR)"; \
		rm ".local/tools/$(LIBGHOSTTY_SPIKE_ZIG_PKG).tar.xz"; \
	fi

# ══════════════════════════════════════════════════════════════════════════════
# Lens (separate red-suite lane — PRD §12/§13/§16)
# ══════════════════════════════════════════════════════════════════════════════

# The lens integration suites carry `test = false` in crates/shux/Cargo.toml, so the default
# `make test` / `make check` never run them; the targets below run them EXPLICITLY. Every suite
# is the SAME nextest invocation — the only variance is the test set, whether a real daemon is
# spawned (→ leak guard + serial `-j 1`), and a couple of extra flags — so that invocation lives
# in ONE place. Adding a suite is a two-line target; the recipe never repeats.
#
#   $(call lens_run,BANNER,--test A [--test B…],[EXTRA FLAGS],[COLOUR])   daemon-backed (guarded)
#   $(call lens_run_pure,BANNER,--test A,[EXTRA FLAGS])                   pure (no daemon)
#
# BANNER must not contain a comma (GNU Make splits $(call) args on commas).
NEXTEST := cargo nextest run -p shux --no-fail-fast
define lens_run
@echo "$(if $(4),$(4),$(COLOR_BLUE))▶ $(1)...$(COLOR_RESET)"
@.shux/scripts/no_leak_guard.sh $(NEXTEST) -j 1 $(3) $(2)
endef
define lens_run_pure
@echo "$(COLOR_BLUE)▶ $(1)...$(COLOR_RESET)"
@$(NEXTEST) $(3) $(2)
endef

LENS_TESTS := --test lens_fixtures_smoke --test lens_glance --test lens_revision \
	--test lens_settle --test lens_diff --test lens_run --test lens_loop

.PHONY: test-lens
test-lens: nextest-ready ## Run the lens synthetic red suite (§12) serially under the leak guard
	$(call lens_run,Running lens red suite (§12),$(LENS_TESTS))

.PHONY: test-lens-settle-hardening
test-lens-settle-hardening: nextest-ready ## Run the task-083 pane.wait_settled hold-ms/stable-frames suite serially under the leak guard
	$(call lens_run,Running lens settle-hardening suite (task 083),--test lens_settle_hardening)

# --no-capture so the loud skip notice (§13) is visible when nidhi/vivecaka are absent (nextest
# otherwise captures the stderr of passing/skipping tests).
.PHONY: test-lens-t
test-lens-t: nextest-ready ## Run the lens T-tier real-TUI suite (§13; loud-skips absent binaries)
	$(call lens_run,Running lens T-tier suite (§13),--test lens_ttier,--no-capture)

.PHONY: test-lens-diff-concurrency
test-lens-diff-concurrency: nextest-ready ## Run the P4 diff concurrent-reader integration test (§7.4 council D2)
	$(call lens_run,Running lens P4 diff concurrency test (§7.4),--test diff_concurrent_readers)

.PHONY: test-lens-scratch-reap
test-lens-scratch-reap: nextest-ready ## Run the P5 scratch reap signal-order test (LENS-R-042, codex B3)
	$(call lens_run,Running lens P5 scratch reap-order test (§8),--test scratch_reap_order)

.PHONY: test-lens-gate
test-lens-gate: nextest-ready ## Run the lens-gate GREEN dogfood suite (task 078; capture on real shux TUIs + cross-path PNG) serially under the leak guard
	$(call lens_run,Running lens-gate dogfood suite (task 078),--test lens_gate_capture)

.PHONY: test-lens-gate-contract
test-lens-gate-contract: nextest-ready ## Run the frozen 078 lens-gate contract lane (GREEN since 081/082 built its cases)
	$(call lens_run,Running the frozen 078 lens-gate contract lane,--test lens_gate_contract)

.PHONY: test-lens-gate-comparator
test-lens-gate-comparator: nextest-ready ## Run the task-079 comparator suite (parity corpus + divergence fixtures + OSC-4 daemon isolation) serially under the leak guard
	$(call lens_run,Running lens-gate comparator suite (task 079),--test lens_gate_parity --test lens_gate_divergence --test diff_palette_isolation)

.PHONY: test-lens-gate-compare
test-lens-gate-compare: nextest-ready ## Run the task-080 golden-compare suite (3 tiers + fingerprint + mask invariance + divergence pixel proofs; PURE, CI-run)
	$(call lens_run_pure,Running lens-gate golden-compare suite (task 080),--test lens_gate_compare)

.PHONY: test-lens-gate-glance-cells
test-lens-gate-glance-cells: nextest-ready ## Run the task-080 daemon-backed `pane.glance --cells` emission suite serially under the leak guard
	$(call lens_run,Running lens-gate glance --cells emission suite (task 080),--test lens_gate_glance_cells)

.PHONY: bench-lens-gate
# opt-level 2 explicitly: `[profile.test]` is 0 so ordinary tests compile fast,
# but this target measures throughput and its numbers are compared against the
# task-080 baseline. An unoptimised build would make them incomparable.
bench-lens-gate: nextest-ready ## Record task-080 capture/compare/render throughput at 10/100/1000 frames (no daemon; prints numbers)
	@CARGO_PROFILE_TEST_OPT_LEVEL=2 $(MAKE) --no-print-directory lens-gate-bench-run

.PHONY: lens-gate-bench-run
lens-gate-bench-run:
	$(call lens_run_pure,Recording lens-gate throughput (task 080 §6),--test lens_gate_bench,--no-capture)

.PHONY: test-lens-gate-run
test-lens-gate-run: nextest-ready ## Run the task-081 scenario-runner suite (drives real fixture TUIs via `shux lens gate`) serially under the leak guard
	$(call lens_run,Running lens-gate scenario-runner suite (task 081),--test lens_gate_run)

.PHONY: test-lens-gate-verdict
test-lens-gate-verdict: nextest-ready ## Run the task-082 verdict/report/xfail/bless/init suite (drives `shux lens gate` end-to-end) serially under the leak guard
	$(call lens_run,Running lens-gate verdict suite (task 082),--test lens_gate_verdict)

.PHONY: test-lens-gate-settle
test-lens-gate-settle: nextest-ready ## Run the task-083 settle-hardening + cast gate suite (drives `shux lens gate` end-to-end) serially under the leak guard
	$(call lens_run,Running lens-gate settle+cast suite (task 083),--test lens_gate_settle)

.PHONY: test-gauntlet-seed
test-gauntlet-seed: ## Seed one task-084 gauntlet cell: make test-gauntlet-seed AGENT=codex CR=cr-b
	@.shux/scripts/laghudarshi_gate_gauntlet.sh seed "$(AGENT)" "$(CR)"

.PHONY: test-gauntlet-run
test-gauntlet-run: ## Drive one cold agent through a seeded cell: make test-gauntlet-run AGENT=codex CR=cr-b
	@.shux/scripts/laghudarshi_gate_agent.sh "$(AGENT)" "$(CR)" $(if $(TIMEOUT),$(TIMEOUT),1800)

.PHONY: test-gauntlet-verify
test-gauntlet-verify: ## Verify one cell from supervisor-observed state: make test-gauntlet-verify AGENT=codex CR=cr-b
	@.shux/scripts/laghudarshi_gate_gauntlet.sh verify "$(AGENT)" "$(CR)"

.PHONY: test-gauntlet-all
test-gauntlet-all: ## Run the whole task-084 gauntlet SERIALLY (6 cells; long). Override with CELLS="codex:cr-b …"
	@.shux/scripts/laghudarshi_gate_all.sh $(if $(CELLS),$(CELLS),codex:cr-b codex:cr-a claude:cr-b claude:cr-a agy:cr-b agy:cr-a)

.PHONY: check-lens-frozen
check-lens-frozen: ## Enforce the lens frozen-path test-integrity trailer (§16.2)
	@bash scripts/check-lens-frozen.sh "$(MSG)"

# ══════════════════════════════════════════════════════════════════════════════
# Code Quality
# ══════════════════════════════════════════════════════════════════════════════

.PHONY: clippy
clippy: ## Run clippy linter
	@echo "$(COLOR_BLUE)▶ Running clippy...$(COLOR_RESET)"
	@cargo clippy --workspace --all-targets -- -D warnings
	@echo "$(COLOR_GREEN)✓ Clippy passed$(COLOR_RESET)"

.PHONY: fmt-check
fmt-check: ## Check formatting (no changes)
	@echo "$(COLOR_BLUE)▶ Checking formatting...$(COLOR_RESET)"
	@cargo fmt --all -- --check
	@echo "$(COLOR_GREEN)✓ Formatting OK$(COLOR_RESET)"

.PHONY: lint
lint: clippy fmt-check ## Run clippy + rustfmt check

.PHONY: fmt
fmt: ## Format all code
	@echo "$(COLOR_BLUE)▶ Formatting code...$(COLOR_RESET)"
	@cargo fmt --all
	@echo "$(COLOR_GREEN)✓ Formatting complete$(COLOR_RESET)"

.PHONY: check
check: hooks-check lint shellcheck check-darwin test check-test-groups check-test-inventory check-ci-parity test-shux-leak-guard test-agent-review-guard check-tui-qa check-gate-docs check-skill-docs check-lens-frozen check-no-bin-mods ## Run lint + test + process/QA guards (what pre-commit runs)
	@echo ""
	@echo "$(COLOR_GREEN)$(COLOR_BOLD)✓ All checks passed!$(COLOR_RESET)"
	@echo ""

.PHONY: ci
ci: lint shellcheck check-darwin test check-test-groups check-test-inventory check-ci-parity test-doc test-shux-leak-guard test-agent-review-guard check-tui-qa check-gate-docs check-skill-docs check-no-bin-mods ## Run the CI pipeline locally (lint + full test + doc tests + process/QA guards)
	@echo ""
	@echo "$(COLOR_GREEN)$(COLOR_BOLD)✓ CI pipeline passed!$(COLOR_RESET)"
	@echo ""

.PHONY: ci-strict
ci-strict: nextest-ready ## Force latest stable toolchain, then run fmt+clippy+build+test (closes version-skew gap)
	@command -v rustup >/dev/null 2>&1 || { echo "$(COLOR_RED)rustup is required for ci-strict (not on PATH)$(COLOR_RESET)" >&2; exit 1; }
	@echo "$(COLOR_BLUE)▶ Updating stable toolchain to latest...$(COLOR_RESET)"
	@rustup update stable
	@echo "$(COLOR_BLUE)▶ Toolchain:$(COLOR_RESET) $$(rustc +stable --version) — $$(cargo +stable clippy --version)"
	@echo ""
	@echo "$(COLOR_BLUE)▶ Format check (+stable)$(COLOR_RESET)"
	@cargo +stable fmt --all -- --check
	@echo "$(COLOR_BLUE)▶ Clippy --all-targets -D warnings (+stable)$(COLOR_RESET)"
	@cargo +stable clippy --workspace --all-targets -- -D warnings
	@echo "$(COLOR_BLUE)▶ Build --all-targets (+stable)$(COLOR_RESET)"
	@cargo +stable build --workspace --all-targets
	@echo "$(COLOR_BLUE)▶ Library tests (+stable)$(COLOR_RESET)"
	@$(NEXTEST_RUN) --workspace --lib
	@echo ""
	@echo "$(COLOR_GREEN)$(COLOR_BOLD)✓ ci-strict passed against $$(rustc +stable --version)$(COLOR_RESET)"
	@echo ""

.PHONY: deny-ready
deny-ready:
	@bash scripts/ensure-deny.sh

.PHONY: deny
deny: deny-ready ## Run license/advisory audit (strict)
	@echo "$(COLOR_BLUE)▶ Running cargo-deny...$(COLOR_RESET)"
	@cargo deny check
	@echo "$(COLOR_GREEN)✓ Audit passed$(COLOR_RESET)"

.PHONY: deny-soft
deny-soft: ## Run license/advisory audit (non-blocking)
	@echo "$(COLOR_BLUE)▶ Running cargo-deny (advisory)...$(COLOR_RESET)"
	@cargo deny check 2>/dev/null || true

.PHONY: check-vt-qa
check-vt-qa: ## Require SOLID QA evidence from any diff that touches VT rendering
	@bash scripts/check-vt-qa-selftest.sh
	@bash scripts/check-vt-qa.sh
	@bash scripts/check-vt-fixtures.sh

.PHONY: shellcheck
shellcheck: ## shellcheck every tracked shell script (the layer the guards live in)
	@bash scripts/check-shell.sh

.PHONY: check-tui-qa
check-tui-qa: ## Verify tracked general TUI QA evidence manifests
	@bash scripts/check-tui-qa.sh

.PHONY: check-gate-docs
check-gate-docs: ## Verify the lens-gate skill reference matches the shipped CLI + THIRD-PARTY-NOTICES
	@bash scripts/check-gate-docs.sh

.PHONY: check-no-bin-mods
check-no-bin-mods: ## Assert crates/shux/src/main.rs declares no modules (lib/bin double-compile)
	@bash scripts/check-no-bin-mods.sh

.PHONY: check-skill-docs
check-skill-docs: ## Verify agent-facing skill gotchas match shipped CLI shapes
	@bash scripts/check-skill-docs.sh

.PHONY: fuzz
fuzz: ## (not wired up) — there is no fuzz/ crate in this repo yet
	@echo "$(COLOR_YELLOW)There is no fuzz/ crate in this repository.$(COLOR_RESET)"
	@echo ""
	@echo "This target used to print four ready-to-run 'cargo fuzz run ...' commands"
	@echo "for targets that do not exist; every one of them failed with"
	@echo "'fuzz directory not found'. Planned under M3 task 056, not yet built."
	@echo ""
	@echo "To start: cargo install cargo-fuzz && cargo fuzz init"

.PHONY: setup
setup: ## Run full dev environment setup
	@echo "$(COLOR_BLUE)▶ Running dev setup...$(COLOR_RESET)"
	@bash scripts/setup-dev.sh
	@echo "$(COLOR_GREEN)✓ Dev environment ready$(COLOR_RESET)"

.PHONY: hooks
hooks: ## Install lefthook git hooks
	@bash scripts/ensure-hooks.sh

.PHONY: hooks-check
hooks-check: ## Fail if this checkout has no git hooks installed
	@bash scripts/ensure-hooks.sh --check

.PHONY: hooks-run
hooks-run: ## Run pre-commit hook manually
	@echo "$(COLOR_BLUE)▶ Running pre-commit hook...$(COLOR_RESET)"
	@lefthook run pre-commit

.PHONY: doc
doc: ## Build documentation
	@echo "$(COLOR_BLUE)▶ Building docs...$(COLOR_RESET)"
	@cargo doc --workspace --no-deps --document-private-items
	@echo "$(COLOR_GREEN)✓ Docs built: target/doc/$(COLOR_RESET)"

.PHONY: clean
clean: ## Clean build artifacts
	@echo "$(COLOR_BLUE)▶ Cleaning...$(COLOR_RESET)"
	@cargo clean
	@rm -rf $(COVERAGE_DIR) lcov.info artifacts staging
	@echo "$(COLOR_GREEN)✓ Cleaned$(COLOR_RESET)"

# ══════════════════════════════════════════════════════════════════════════════
# Release
# ══════════════════════════════════════════════════════════════════════════════

.PHONY: release-build
release-build: ## Build host-target release binary into target/<triple>/release/
	@echo "$(COLOR_BLUE)▶ Building release binary for host...$(COLOR_RESET)"
	@HOST_TRIPLE=$$(rustc -vV | awk '/^host:/{print $$2}'); \
		cargo build --release --bin shux --target $${HOST_TRIPLE} && \
		echo "$(COLOR_GREEN)✓ Built target/$${HOST_TRIPLE}/release/shux$(COLOR_RESET)"

.PHONY: release-package
release-package: ## Package whatever target/<triple>/release/shux exists into artifacts/
	@VERSION=$$(grep -m1 'version = ' Cargo.toml | sed 's/.*"\(.*\)".*/\1/'); \
		echo "$(COLOR_BLUE)▶ Packaging shux v$${VERSION} (HOST_ONLY)...$(COLOR_RESET)"; \
		HOST_ONLY=1 ./scripts/build-release.sh "$${VERSION}"
	@echo "$(COLOR_GREEN)✓ Artifacts in ./artifacts/$(COLOR_RESET)"

.PHONY: version
version: ## Show version info
	@echo "$(COLOR_MAGENTA)Binary:       $(BINARY_NAME)$(COLOR_RESET)"
	@echo "$(COLOR_MAGENTA)Version:      $(VERSION)$(COLOR_RESET)"
	@echo "$(COLOR_MAGENTA)Commit:       $(COMMIT)$(COLOR_RESET)"
	@echo "$(COLOR_MAGENTA)Rust Version: $(RUST_VERSION)$(COLOR_RESET)"

.PHONY: info
info: ## Show project info
	@echo ""
	@echo "$(COLOR_BOLD)shux$(COLOR_RESET) — modern terminal multiplexer"
	@echo ""
	@echo "$(COLOR_BLUE)Repository:$(COLOR_RESET)   https://github.com/indrasvat/shux"
	@echo "$(COLOR_BLUE)Rust:$(COLOR_RESET)         $(RUST_VERSION)"
	@echo "$(COLOR_BLUE)Build:$(COLOR_RESET)        $(VERSION) ($(COMMIT))"
	@echo ""
	@echo "$(COLOR_YELLOW)Architecture:$(COLOR_RESET)"
	@echo "  crates/shux/         CLI entrypoint (clap, daemon auto-start)"
	@echo "  crates/shux-core/    Core engine (SessionGraph, LayoutEngine, EventBus)"
	@echo "  crates/shux-pty/     PTY manager (pty-process, async I/O)"
	@echo "  crates/shux-vt/     Virtual terminal grid (vte, VecDeque)"
	@echo "  crates/shux-rpc/     JSON-RPC server (UDS + TCP)"
	@echo "  crates/shux-plugin/  Plugin host (process plugins over stdio JSON-RPC)"
	@echo "  crates/shux-ui/      TUI client (crossterm, hand-rolled chrome)"
	@echo ""
