# Makefile for shux
# Modern, batteries-included terminal multiplexer

# ══════════════════════════════════════════════════════════════════════════════
# Variables
# ══════════════════════════════════════════════════════════════════════════════

BINARY_NAME := shux
VERSION := $(shell cargo metadata --format-version 1 --no-deps 2>/dev/null | grep -o '"version":"[^"]*"' | head -1 | cut -d'"' -f4 || echo "dev")
COMMIT := $(shell git rev-parse --short HEAD 2>/dev/null || echo "unknown")
RUST_VERSION := $(shell rustc --version 2>/dev/null | cut -d' ' -f2 || echo "unknown")

# Directories
COVERAGE_DIR := coverage

# Tools
NEXTEST := $(shell command -v cargo-nextest 2>/dev/null)
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
	@awk 'BEGIN {FS = ":.*##"} /^(clippy|lint|fmt|check|ci|deny|fuzz)[a-zA-Z_-]*:.*?##/ { printf "  $(COLOR_GREEN)%-20s$(COLOR_RESET) %s\n", $$1, $$2 }' $(MAKEFILE_LIST)
	@echo ""
	@echo "$(COLOR_BOLD)Tooling:$(COLOR_RESET)"
	@awk 'BEGIN {FS = ":.*##"} /^(setup|hooks|doc|clean|version|info)[a-zA-Z_-]*:.*?##/ { printf "  $(COLOR_GREEN)%-20s$(COLOR_RESET) %s\n", $$1, $$2 }' $(MAKEFILE_LIST)
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

.PHONY: install-tools
install-tools: ## Install dev dependencies (nextest, llvm-cov, deny, fuzz, lefthook)
	@echo "$(COLOR_BLUE)▶ Installing dev tools...$(COLOR_RESET)"
	cargo install cargo-nextest --locked
	cargo install cargo-llvm-cov --locked
	cargo install cargo-deny --locked
	cargo install cargo-fuzz --locked
	cargo install lefthook --locked || npm i -g lefthook
	@echo "$(COLOR_GREEN)✓ Dev tools installed$(COLOR_RESET)"

# ══════════════════════════════════════════════════════════════════════════════
# Test
# ══════════════════════════════════════════════════════════════════════════════

.PHONY: test
test: ## Run all tests with cargo-nextest
	@echo "$(COLOR_BLUE)▶ Running tests...$(COLOR_RESET)"
	@cargo nextest run --workspace --no-tests=pass
	@echo "$(COLOR_GREEN)✓ Tests passed$(COLOR_RESET)"

.PHONY: test-verbose
test-verbose: ## Run tests with output visible
	@echo "$(COLOR_BLUE)▶ Running tests (verbose)...$(COLOR_RESET)"
	@cargo nextest run --workspace --no-capture --no-tests=pass

.PHONY: test-lib
test-lib: ## Run library tests only
	@echo "$(COLOR_BLUE)▶ Running library tests...$(COLOR_RESET)"
	@cargo nextest run --workspace --lib --no-tests=pass
	@echo "$(COLOR_GREEN)✓ Library tests passed$(COLOR_RESET)"

.PHONY: test-doc
test-doc: ## Run doc tests
	@echo "$(COLOR_BLUE)▶ Running doc tests...$(COLOR_RESET)"
	@cargo test --workspace --doc
	@echo "$(COLOR_GREEN)✓ Doc tests passed$(COLOR_RESET)"

.PHONY: test-coverage
test-coverage: ## Run tests with coverage report
	@echo "$(COLOR_BLUE)▶ Running tests with coverage...$(COLOR_RESET)"
	@mkdir -p $(COVERAGE_DIR)
	@cargo llvm-cov nextest --workspace --lcov --output-path $(COVERAGE_DIR)/lcov.info
	@echo "$(COLOR_GREEN)✓ Coverage report: $(COVERAGE_DIR)/lcov.info$(COLOR_RESET)"

.PHONY: bench
bench: ## Run benchmarks
	@echo "$(COLOR_BLUE)▶ Running benchmarks...$(COLOR_RESET)"
	@cargo bench --workspace

.PHONY: bench-baseline
bench-baseline: ## Record M0 performance baseline
	@./scripts/bench-baseline.sh

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
check: lint test ## Run lint + test (what pre-commit runs)
	@echo ""
	@echo "$(COLOR_GREEN)$(COLOR_BOLD)✓ All checks passed!$(COLOR_RESET)"
	@echo ""

.PHONY: ci
ci: lint test-lib test-doc ## Run CI pipeline (lint + test-lib + test-doc)
	@echo ""
	@echo "$(COLOR_GREEN)$(COLOR_BOLD)✓ CI pipeline passed!$(COLOR_RESET)"
	@echo ""

.PHONY: deny
deny: ## Run license/advisory audit (strict)
	@echo "$(COLOR_BLUE)▶ Running cargo-deny...$(COLOR_RESET)"
	@cargo deny check
	@echo "$(COLOR_GREEN)✓ Audit passed$(COLOR_RESET)"

.PHONY: deny-soft
deny-soft: ## Run license/advisory audit (non-blocking)
	@echo "$(COLOR_BLUE)▶ Running cargo-deny (advisory)...$(COLOR_RESET)"
	@cargo deny check 2>/dev/null || true

.PHONY: check-progress
check-progress: ## Verify PROGRESS.md and task Status fields are updated
	@bash scripts/check-progress.sh

.PHONY: check-progress-active
check-progress-active: ## Verify progress (active session variant, allows In Progress)
	@bash scripts/check-progress.sh --active-session

.PHONY: fuzz
fuzz: ## Show available fuzz targets
	@echo "$(COLOR_YELLOW)Run individual fuzz targets with: cargo fuzz run <target>$(COLOR_RESET)"
	@echo ""
	@echo "Available targets (after M3 task 056):"
	@echo "  $(COLOR_GREEN)cargo fuzz run fuzz_vt_parser$(COLOR_RESET)"
	@echo "  $(COLOR_GREEN)cargo fuzz run fuzz_json_rpc$(COLOR_RESET)"
	@echo "  $(COLOR_GREEN)cargo fuzz run fuzz_config$(COLOR_RESET)"
	@echo "  $(COLOR_GREEN)cargo fuzz run fuzz_layout$(COLOR_RESET)"

# ══════════════════════════════════════════════════════════════════════════════
# Tooling
# ══════════════════════════════════════════════════════════════════════════════

.PHONY: setup
setup: ## Run full dev environment setup
	@echo "$(COLOR_BLUE)▶ Running dev setup...$(COLOR_RESET)"
	@bash scripts/setup-dev.sh
	@echo "$(COLOR_GREEN)✓ Dev environment ready$(COLOR_RESET)"

.PHONY: hooks
hooks: ## Install lefthook git hooks
	@echo "$(COLOR_BLUE)▶ Installing git hooks...$(COLOR_RESET)"
	@lefthook install
	@echo "$(COLOR_GREEN)✓ Git hooks installed$(COLOR_RESET)"

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
	@rm -rf $(COVERAGE_DIR) lcov.info
	@echo "$(COLOR_GREEN)✓ Cleaned$(COLOR_RESET)"

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
	@echo "  crates/shux-plugin/  Plugin host (wasmtime, WIT, process plugins)"
	@echo "  crates/shux-ui/      TUI client (crossterm, ratatui)"
	@echo ""
