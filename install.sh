#!/usr/bin/env bash
set -euo pipefail

# shux installer
# Usage: curl -sSfL https://raw.githubusercontent.com/indrasvat/shux/main/install.sh | bash
#        curl ... | bash -s -- --version v0.1.0 --dir ~/.local/bin

REPO="indrasvat/shux"
BINARY="shux"
DEFAULT_DIR="${HOME}/.local/bin"
SKILL_PACKAGE="indrasvat/shux"

# --- Colors (Catppuccin-leaning palette: cyan accent on warm subtext) ---------

setup_colors() {
    if [[ -n "${NO_COLOR:-}" ]] || [[ ! -t 1 ]]; then
        BOLD=""
        RESET=""
        ACCENT=""
        TEAL=""
        GREEN=""
        RED=""
        YELLOW=""
        TEXT=""
        SUBTEXT=""
    else
        BOLD=$'\033[1m'
        RESET=$'\033[0m'
        # Catppuccin Macchiato Sapphire (#74c7ec) — primary accent
        ACCENT=$'\033[38;2;116;199;236m'
        # Catppuccin Macchiato Teal (#8bd5ca)
        TEAL=$'\033[38;2;139;213;202m'
        # Catppuccin Macchiato Green (#a6da95)
        GREEN=$'\033[38;2;166;218;149m'
        # Catppuccin Macchiato Red (#ed8796)
        RED=$'\033[38;2;237;135;150m'
        # Catppuccin Macchiato Yellow (#eed49f)
        YELLOW=$'\033[38;2;238;212;159m'
        # Catppuccin Macchiato Text (#cad3f5)
        TEXT=$'\033[38;2;202;211;245m'
        # Catppuccin Macchiato Subtext1 (#b8c0e0)
        SUBTEXT=$'\033[38;2;91;96;120m'
    fi
}

banner() {
    local cols
    cols=$(tput cols 2>/dev/null || echo 80)
    if [[ "${cols}" -lt 60 ]]; then
        return
    fi
    printf '\n'
    printf '%s' "${ACCENT}"
    printf '   ███████╗ ██╗  ██╗ ██╗   ██╗ ██╗  ██╗\n'
    printf '   ██╔════╝ ██║  ██║ ██║   ██║ ╚██╗██╔╝\n'
    printf '   ███████╗ ███████║ ██║   ██║  ╚███╔╝ \n'
    printf '   ╚════██║ ██╔══██║ ██║   ██║  ██╔██╗ \n'
    printf '   ███████║ ██║  ██║ ╚██████╔╝ ██╔╝ ██╗\n'
    printf '   ╚══════╝ ╚═╝  ╚═╝  ╚═════╝  ╚═╝  ╚═╝\n'
    printf '%s' "${RESET}"
    printf '%s     a typed-API multiplexer for humans and agents%s\n\n' "${SUBTEXT}" "${RESET}"
}

info()       { printf '  %s→%s %s%s%s\n' "${TEAL}" "${RESET}" "${TEXT}" "$1" "${RESET}"; }
success()    { printf '  %s✓%s %s%s%s\n' "${GREEN}" "${RESET}" "${TEXT}" "$1" "${RESET}"; }
warn()       { printf '  %s! %s%s\n' "${YELLOW}" "$1" "${RESET}"; }
error_exit() { printf '  %s✗ %s%s\n' "${RED}" "$1" "${RESET}" >&2; exit 1; }

step() {
    local n="$1" total="$2" msg="$3"
    printf '\n%s%s[%s/%s]%s %s%s%s%s\n' "${BOLD}" "${ACCENT}" "${n}" "${total}" "${RESET}" "${BOLD}" "${TEXT}" "${msg}" "${RESET}"
}

# --- Argument parsing ---------------------------------------------------------

usage() {
    printf '%s%sshux installer%s\n\n' "${BOLD}" "${TEXT}" "${RESET}"
    printf '%sUsage:%s\n' "${SUBTEXT}" "${RESET}"
    printf '  curl -sSfL https://raw.githubusercontent.com/%s/main/install.sh | bash\n' "${REPO}"
    printf '  curl ... | bash -s -- [OPTIONS]\n\n'
    printf '%sOptions:%s\n' "${SUBTEXT}" "${RESET}"
    printf '  %s--version VERSION%s  Install specific version (e.g. v0.1.0)\n' "${TEXT}" "${RESET}"
    printf '  %s--dir DIRECTORY%s    Install directory (default: %s)\n' "${TEXT}" "${RESET}" "${DEFAULT_DIR}"
    printf '  %s--no-skill%s         Skip the agent skill install (npx skills add ...)\n' "${TEXT}" "${RESET}"
    printf '  %s--help%s             Show this help\n' "${TEXT}" "${RESET}"
    exit 0
}

parse_args() {
    VERSION=""
    INSTALL_DIR="${DEFAULT_DIR}"
    INSTALL_SKILL=1

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --version)
                if [[ $# -lt 2 ]]; then error_exit "--version requires a value"; fi
                VERSION="$2"
                shift 2
                ;;
            --dir)
                if [[ $# -lt 2 ]]; then error_exit "--dir requires a value"; fi
                INSTALL_DIR="$2"
                shift 2
                ;;
            --no-skill)
                INSTALL_SKILL=0
                shift
                ;;
            --help)
                usage
                ;;
            *)
                error_exit "Unknown option: $1 (use --help for usage)"
                ;;
        esac
    done
}

# --- Dependency checks --------------------------------------------------------

check_dependencies() {
    if command -v curl >/dev/null 2>&1; then
        DOWNLOADER="curl"
        success "Using curl for downloads"
    elif command -v wget >/dev/null 2>&1; then
        DOWNLOADER="wget"
        success "Using wget for downloads"
    else
        error_exit "curl or wget is required"
    fi

    if command -v shasum >/dev/null 2>&1; then
        HASHER="shasum"
        success "Using shasum for verification"
    elif command -v sha256sum >/dev/null 2>&1; then
        HASHER="sha256sum"
        success "Using sha256sum for verification"
    else
        error_exit "shasum or sha256sum is required"
    fi

    if ! command -v tar >/dev/null 2>&1; then
        error_exit "tar is required"
    fi
}

# --- Platform detection -------------------------------------------------------

detect_platform() {
    local os arch

    os="$(uname -s)"
    case "${os}" in
        Darwin) OS="darwin" ;;
        Linux)  OS="linux" ;;
        *)      error_exit "Unsupported operating system: ${os} (Windows users: download from the Releases page)" ;;
    esac

    arch="$(uname -m)"
    case "${arch}" in
        x86_64)  ARCH="x86_64" ;;
        amd64)   ARCH="x86_64" ;;
        arm64)   ARCH="aarch64" ;;
        aarch64) ARCH="aarch64" ;;
        *)       error_exit "Unsupported architecture: ${arch}" ;;
    esac

    success "Platform: ${OS}/${ARCH}"
}

# --- Version resolution -------------------------------------------------------

# fetch_with_status writes the response body to $1 and prints exactly
# one line — the HTTP status code (e.g. "200", "404"), or "000" on
# transport failure (DNS, refused connection, TLS). Always exits 0 so
# the caller drives logic from the status string.
#
# Note on curl: -sSL (no -f) keeps curl's exit code at 0 even on HTTP
# 4xx/5xx so the status code we capture in -w is the truth. On real
# network failure curl writes "000" via -w *and* exits non-zero — we
# capture the substitution into a variable so we never duplicate the
# "000" with a fallback.
fetch_with_status() {
    local outfile="$1" url="$2"
    local code=""
    if [[ "${DOWNLOADER}" == "curl" ]]; then
        code=$(curl -sSL -o "${outfile}" -w '%{http_code}' "${url}" 2>/dev/null) || code=""
        echo "${code:-000}"
    else
        # wget swallows status. Treat success as 200, failure as 000.
        if wget -qO "${outfile}" "${url}" 2>/dev/null; then
            echo "200"
        else
            echo "000"
        fi
    fi
}

# parse_first_tag pulls the first tag_name out of a /releases or
# /releases/latest JSON response. /releases is newest-first by default,
# so the first match is the most recent release including pre-releases.
parse_first_tag() {
    grep -m1 '"tag_name"' | sed -E 's/.*"tag_name": *"([^"]+)".*/\1/'
}

# get_latest_version sets VERSION. Falls back from /releases/latest to
# /releases?per_page=1 ONLY on a 404 (no stable release yet). Network
# errors and rate-limiting (403/5xx) abort with a clear message instead
# of silently installing a prerelease.
get_latest_version() {
    local body status tag

    # RETURN trap leaks past function boundary without `set -T` — write
    # to the script tmpdir so the EXIT trap sweeps it.
    body="$(mktemp "${TMPDIR_CREATED}/shux-api-body.XXXXXX")"

    status="$(fetch_with_status "${body}" "https://api.github.com/repos/${REPO}/releases/latest")"

    case "${status}" in
        200)
            tag="$(parse_first_tag < "${body}" || true)"
            ;;
        404)
            # No stable release yet — try the prerelease endpoint.
            status="$(fetch_with_status "${body}" "https://api.github.com/repos/${REPO}/releases?per_page=1")"
            if [[ "${status}" != "200" ]]; then
                error_exit "GitHub API returned HTTP ${status} fetching prereleases. Pin a version with --version vX.Y.Z."
            fi
            tag="$(parse_first_tag < "${body}" || true)"
            if [[ -n "${tag}" ]]; then
                warn "No stable release yet — installing pre-release ${tag}"
            fi
            ;;
        403|429)
            error_exit "GitHub API rate-limited (HTTP ${status}). Pin a version with --version vX.Y.Z to skip the lookup."
            ;;
        000)
            error_exit "Network error fetching the latest release. Check your connection."
            ;;
        *)
            error_exit "GitHub API returned HTTP ${status} fetching the latest release."
            ;;
    esac

    if [[ -z "${tag:-}" ]]; then
        error_exit "Could not find a release at github.com/${REPO}/releases. Pin a version with --version vX.Y.Z if a tag exists."
    fi
    VERSION="${tag}"
}

# --- Download helpers ---------------------------------------------------------

build_download_url() {
    # Asset names produced by scripts/build-release.sh:
    #   shux-v<version>-<arch>-<os>.tar.gz
    #   shux-v<version>-<arch>-<os>.tar.gz.sha256
    local version_no_v="${VERSION#v}"
    TARBALL="shux-v${version_no_v}-${ARCH}-${OS}.tar.gz"
    TARBALL_URL="https://github.com/${REPO}/releases/download/${VERSION}/${TARBALL}"
    CHECKSUM_URL="https://github.com/${REPO}/releases/download/${VERSION}/${TARBALL}.sha256"
    EXTRACTED_DIR="shux-v${version_no_v}-${ARCH}-${OS}"
}

download_file() {
    local url="$1" dest="$2"
    if [[ "${DOWNLOADER}" == "curl" ]]; then
        curl -sSfL -o "${dest}" "${url}" 2>/dev/null
    else
        wget -q -O "${dest}" "${url}" 2>/dev/null
    fi
}

# --- Checksum verification ----------------------------------------------------

verify_checksum() {
    local checksum_file="$1" tarball_file="$2"
    local expected actual

    expected="$(awk '{print $1}' < "${checksum_file}" | head -1)"
    if [[ -z "${expected}" ]]; then
        error_exit "Could not parse checksum from ${checksum_file}"
    fi

    if [[ "${HASHER}" == "shasum" ]]; then
        actual="$(shasum -a 256 "${tarball_file}" | awk '{print $1}')"
    else
        actual="$(sha256sum "${tarball_file}" | awk '{print $1}')"
    fi

    if [[ "${expected}" != "${actual}" ]]; then
        error_exit "Checksum mismatch! Expected ${expected}, got ${actual}"
    fi
}

# --- Installation -------------------------------------------------------------

install_binary() {
    local tmpdir="$1"
    tar -xzf "${tmpdir}/${TARBALL}" -C "${tmpdir}"

    local src="${tmpdir}/${EXTRACTED_DIR}/${BINARY}"
    if [[ ! -f "${src}" ]]; then
        error_exit "Binary not found in tarball at ${EXTRACTED_DIR}/${BINARY}"
    fi

    mkdir -p "${INSTALL_DIR}"
    install -m 755 "${src}" "${INSTALL_DIR}/${BINARY}"
}

# --- Agent skill (optional) ---------------------------------------------------

# install_skill installs the shux agent skill globally via npx so
# agents (Claude Code, Codex, etc.) can discover it. Best-effort:
# no npx → friendly skip; install fails → warn and keep going. The
# binary is the source of truth, the skill is just the agent
# onboarding sugar.
install_skill() {
    if [[ "${INSTALL_SKILL}" -ne 1 ]]; then
        info "Skipping agent skill install (--no-skill)"
        return
    fi

    if ! command -v npx >/dev/null 2>&1; then
        warn "npx not found — skipping agent skill"
        info "Install later with: ${BOLD}npx skills add ${SKILL_PACKAGE} --global --yes${RESET}"
        return
    fi

    info "Running: npx skills add ${SKILL_PACKAGE} --global --yes"
    if npx --yes skills add "${SKILL_PACKAGE}" --global --yes >/dev/null 2>&1; then
        success "Agent skill installed (use 'shux' from any AI agent)"
    else
        warn "npx skills add failed — install later with the same command"
    fi
}

check_path() {
    case ":${PATH}:" in
        *":${INSTALL_DIR}:"*) return ;;
    esac

    warn "${INSTALL_DIR} is not in your PATH"

    local shell_name rc_file
    shell_name="$(basename "${SHELL:-/bin/bash}")"
    # shellcheck disable=SC2088  # tilde is intentional for display in `info` message
    case "${shell_name}" in
        zsh)  rc_file="~/.zshrc" ;;
        bash) rc_file="~/.bashrc" ;;
        fish) rc_file="~/.config/fish/config.fish" ;;
        *)    rc_file="your shell config" ;;
    esac

    info "Add this to ${rc_file}:"
    # shellcheck disable=SC2016  # $PATH is literal display text, not expansion
    printf '\n  %sexport PATH="%s:$PATH"%s\n\n' "${SUBTEXT}" "${INSTALL_DIR}" "${RESET}"
}

# --- Cleanup ------------------------------------------------------------------

cleanup() {
    if [[ -n "${TMPDIR_CREATED:-}" ]]; then
        rm -rf "${TMPDIR_CREATED}"
    fi
}

# --- Main ---------------------------------------------------------------------

main() {
    setup_colors
    banner
    parse_args "$@"

    # Set up tmpdir + EXIT trap before any function that mktemp's into it.
    local tmpdir
    tmpdir="$(mktemp -d -t shux-install.XXXXXX)" \
        || error_exit "Could not create temporary directory"
    TMPDIR_CREATED="${tmpdir}"
    trap cleanup EXIT INT TERM HUP

    step 1 7 "Checking dependencies"
    check_dependencies

    step 2 7 "Detecting platform"
    detect_platform

    step 3 7 "Finding release"
    if [[ -n "${VERSION}" ]]; then
        success "Version: ${VERSION} (requested)"
    else
        get_latest_version
        success "Version: ${VERSION} (latest)"
    fi

    build_download_url

    step 4 7 "Downloading ${BINARY}"
    download_file "${TARBALL_URL}" "${tmpdir}/${TARBALL}" \
        || error_exit "Download failed. Check that ${VERSION} exists at github.com/${REPO}/releases"
    success "Downloaded ${TARBALL}"

    step 5 7 "Verifying checksum"
    download_file "${CHECKSUM_URL}" "${tmpdir}/${TARBALL}.sha256" \
        || error_exit "Failed to download ${TARBALL}.sha256"
    verify_checksum "${tmpdir}/${TARBALL}.sha256" "${tmpdir}/${TARBALL}"
    success "Checksum verified (SHA-256)"

    step 6 7 "Installing to ${INSTALL_DIR}"
    install_binary "${tmpdir}"
    success "Installed ${BINARY} ${VERSION}"

    # macOS: clear Gatekeeper quarantine flag from a downloaded binary so
    # users don't have to right-click → Open the first time.
    if [[ "${OS}" == "darwin" ]]; then
        xattr -d com.apple.quarantine "${INSTALL_DIR}/${BINARY}" 2>/dev/null || true
        success "Cleared macOS quarantine flag"
    fi

    step 7 7 "Agent skill"
    install_skill

    printf '\n  %s✓%s %s%sInstallation complete!%s\n\n' "${GREEN}" "${RESET}" "${BOLD}" "${TEXT}" "${RESET}"

    check_path

    info "Run ${BOLD}shux${RESET}${TEXT} to attach to (or create) the default session"
    info "List sessions: ${BOLD}shux session list${RESET}"
    info "JSON-RPC API:  ${BOLD}shux rpc call session.list --params '{}'${RESET}"
    printf '\n'
}

main "$@"
