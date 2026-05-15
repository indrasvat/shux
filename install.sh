#!/bin/sh
# shux installer — POSIX-portable so `curl ... | sh` works under any
# POSIX shell (bash, dash, ash, busybox sh, ksh, zsh). shux itself
# imposes no shell requirements on its users; the installer matches.
#
# Usage: curl -sSfL https://shux.pages.dev/install.sh | sh
#        curl ... | sh -s -- --version v0.1.0 --dir ~/.local/bin
set -eu

REPO="indrasvat/shux"
BINARY="shux"
DEFAULT_DIR="${HOME}/.local/bin"
SKILL_PACKAGE="indrasvat/shux"

# --- Colors (Catppuccin-leaning palette: cyan accent on warm subtext) ---------
#
# POSIX shells lack `$'\033[...'` ANSI-C quoting, so we build the ESC
# char once via `printf` and concatenate. Works identically across
# bash, dash, ash, busybox sh.

setup_colors() {
    if [ -n "${NO_COLOR:-}" ] || [ ! -t 1 ]; then
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
        ESC=$(printf '\033')
        BOLD="${ESC}[1m"
        RESET="${ESC}[0m"
        # Catppuccin Macchiato Sapphire (#74c7ec) — primary accent
        ACCENT="${ESC}[38;2;116;199;236m"
        # Catppuccin Macchiato Teal (#8bd5ca)
        TEAL="${ESC}[38;2;139;213;202m"
        # Catppuccin Macchiato Green (#a6da95)
        GREEN="${ESC}[38;2;166;218;149m"
        # Catppuccin Macchiato Red (#ed8796)
        RED="${ESC}[38;2;237;135;150m"
        # Catppuccin Macchiato Yellow (#eed49f)
        YELLOW="${ESC}[38;2;238;212;159m"
        # Catppuccin Macchiato Text (#cad3f5)
        TEXT="${ESC}[38;2;202;211;245m"
        # Catppuccin Macchiato Subtext1 (#b8c0e0)
        SUBTEXT="${ESC}[38;2;91;96;120m"
    fi
}

banner() {
    cols=$(tput cols 2>/dev/null || echo 80)
    if [ "${cols}" -lt 60 ]; then
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
    n="$1"
    total="$2"
    msg="$3"
    printf '\n%s%s[%s/%s]%s %s%s%s%s\n' "${BOLD}" "${ACCENT}" "${n}" "${total}" "${RESET}" "${BOLD}" "${TEXT}" "${msg}" "${RESET}"
}

# --- Argument parsing ---------------------------------------------------------

usage() {
    printf '%s%sshux installer%s\n\n' "${BOLD}" "${TEXT}" "${RESET}"
    printf '%sUsage:%s\n' "${SUBTEXT}" "${RESET}"
    printf '  curl -sSfL https://shux.pages.dev/install.sh | sh\n'
    printf '  curl ... | sh -s -- [OPTIONS]\n\n'
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

    while [ $# -gt 0 ]; do
        case "$1" in
            --version)
                if [ $# -lt 2 ]; then error_exit "--version requires a value"; fi
                VERSION="$2"
                shift 2
                ;;
            --dir)
                if [ $# -lt 2 ]; then error_exit "--dir requires a value"; fi
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
    outfile="$1"
    url="$2"
    code=""
    if [ "${DOWNLOADER}" = "curl" ]; then
        code=$(curl -sSL -o "${outfile}" -w '%{http_code}' "${url}" 2>/dev/null) || code=""
        echo "${code:-000}"
    else
        # wget doesn't expose %{http_code} directly. -S prints the
        # response status line to stderr ("HTTP/1.1 403 Forbidden");
        # capture that and parse out the code. We can't use -q with -S
        # because -q would suppress -S's output. Fall through to "000"
        # if we couldn't extract a code (genuine network failure).
        # GNU wget and busybox wget both support -S since ~2016.
        wget_stderr=$(wget -S -O "${outfile}" "${url}" 2>&1 1>/dev/null) || true
        code=$(printf '%s\n' "${wget_stderr}" | awk '
            /^[[:space:]]*HTTP\// { last = $2 }
            END { print (last ? last : "000") }
        ')
        echo "${code:-000}"
    fi
}

# parse_first_tag pulls the first tag_name out of a /releases or
# /releases/latest JSON response. /releases is newest-first by default,
# so the first match is the most recent release including pre-releases.
parse_first_tag() {
    grep -m1 '"tag_name"' | sed -E 's/.*"tag_name": *"([^"]+)".*/\1/'
}

# get_latest_tag_from_web resolves "the latest stable release" via the
# `https://github.com/<repo>/releases/latest` web redirect — NOT the
# rate-limited api.github.com endpoint. The web URL 302's to
# /releases/tag/v<X.Y.Z> for the latest stable; we grab the redirect
# target with curl's `-w '%{redirect_url}'` (no HTTP parsing, no awk,
# no sed) and pull the tag out with POSIX parameter expansion. Web
# traffic has much more generous rate limits than the API, so this
# is the right primary path for anonymous installs.
#
# Returns the tag on stdout (e.g. `v0.22.0`), or exits non-zero with
# no output when the lookup can't yield a tag (curl failure, 404
# because the repo has no stable release yet, unexpected URL shape).
# The caller falls back to the API in that case.
#
# wget doesn't expose `redirect_url` cleanly, so this is curl-only;
# wget users skip straight to the API path.
get_latest_tag_from_web() {
    [ "${DOWNLOADER}" = "curl" ] || return 1
    url=$(curl -fsSI -o /dev/null -w '%{redirect_url}' \
        "https://github.com/${REPO}/releases/latest" 2>/dev/null) || return 1
    case "$url" in
        */tag/*) ;;
        *) return 1 ;;
    esac
    tag=${url##*/tag/}
    # Strip any trailing CR / whitespace / non-tag noise. POSIX
    # parameter expansion: longest suffix match of "one non-tag char
    # followed by anything" — gives `v0.22.0` from `v0.22.0\r`.
    tag=${tag%%[!A-Za-z0-9._+-]*}
    [ -n "$tag" ] || return 1
    printf '%s\n' "$tag"
}

# get_latest_version sets VERSION. Primary path is the web redirect
# (no API quota burned). Falls back to api.github.com only when the
# web lookup yields nothing — that's the prerelease case (repo has
# no stable release yet) or wget users. Then within the API path,
# /releases/latest falls back to /releases?per_page=1 on 404 to
# surface prereleases. Network errors and API rate-limiting (403/5xx)
# abort with a clear message pointing at --version vX.Y.Z.
get_latest_version() {
    web_tag=$(get_latest_tag_from_web 2>/dev/null) || web_tag=""
    if [ -n "$web_tag" ]; then
        VERSION="$web_tag"
        return
    fi

    body="$(mktemp "${TMPDIR_CREATED}/shux-api-body.XXXXXX")"

    status="$(fetch_with_status "${body}" "https://api.github.com/repos/${REPO}/releases/latest")"

    case "${status}" in
        200)
            tag="$(parse_first_tag < "${body}" || true)"
            ;;
        404)
            # No stable release yet — try the prerelease endpoint.
            status="$(fetch_with_status "${body}" "https://api.github.com/repos/${REPO}/releases?per_page=1")"
            if [ "${status}" != "200" ]; then
                error_exit "GitHub API returned HTTP ${status} fetching prereleases. Pin a version with --version vX.Y.Z."
            fi
            tag="$(parse_first_tag < "${body}" || true)"
            if [ -n "${tag}" ]; then
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

    if [ -z "${tag:-}" ]; then
        error_exit "Could not find a release at github.com/${REPO}/releases. Pin a version with --version vX.Y.Z if a tag exists."
    fi
    VERSION="${tag}"
}

# --- Download helpers ---------------------------------------------------------

build_download_url() {
    # Asset names produced by scripts/build-release.sh:
    #   shux-v<version>-<arch>-<os>.tar.gz
    #   shux-v<version>-<arch>-<os>.tar.gz.sha256
    version_no_v="${VERSION#v}"
    TARBALL="shux-v${version_no_v}-${ARCH}-${OS}.tar.gz"
    TARBALL_URL="https://github.com/${REPO}/releases/download/${VERSION}/${TARBALL}"
    CHECKSUM_URL="https://github.com/${REPO}/releases/download/${VERSION}/${TARBALL}.sha256"
    EXTRACTED_DIR="shux-v${version_no_v}-${ARCH}-${OS}"
}

download_file() {
    url="$1"
    dest="$2"
    if [ "${DOWNLOADER}" = "curl" ]; then
        curl -sSfL -o "${dest}" "${url}" 2>/dev/null
    else
        wget -q -O "${dest}" "${url}" 2>/dev/null
    fi
}

# --- Checksum verification ----------------------------------------------------

verify_checksum() {
    checksum_file="$1"
    tarball_file="$2"

    expected="$(awk '{print $1}' < "${checksum_file}" | head -1)"
    if [ -z "${expected}" ]; then
        error_exit "Could not parse checksum from ${checksum_file}"
    fi

    if [ "${HASHER}" = "shasum" ]; then
        actual="$(shasum -a 256 "${tarball_file}" | awk '{print $1}')"
    else
        actual="$(sha256sum "${tarball_file}" | awk '{print $1}')"
    fi

    if [ "${expected}" != "${actual}" ]; then
        error_exit "Checksum mismatch! Expected ${expected}, got ${actual}"
    fi
}

# --- Installation -------------------------------------------------------------

install_binary() {
    tmpdir="$1"
    tar -xzf "${tmpdir}/${TARBALL}" -C "${tmpdir}"

    src="${tmpdir}/${EXTRACTED_DIR}/${BINARY}"
    if [ ! -f "${src}" ]; then
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
    if [ "${INSTALL_SKILL}" -ne 1 ]; then
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

    shell_name="$(basename "${SHELL:-/bin/sh}")"
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
    if [ -n "${TMPDIR_CREATED:-}" ]; then
        rm -rf "${TMPDIR_CREATED}"
    fi
}

# --- Main ---------------------------------------------------------------------

main() {
    setup_colors
    banner
    parse_args "$@"

    # Set up tmpdir + EXIT trap before any function that mktemp's into it.
    tmpdir="$(mktemp -d -t shux-install.XXXXXX 2>/dev/null || mktemp -d)" \
        || error_exit "Could not create temporary directory"
    TMPDIR_CREATED="${tmpdir}"
    trap cleanup EXIT INT TERM HUP

    step 1 7 "Checking dependencies"
    check_dependencies

    step 2 7 "Detecting platform"
    detect_platform

    step 3 7 "Finding release"
    if [ -n "${VERSION}" ]; then
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
    if [ "${OS}" = "darwin" ]; then
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
