#!/bin/sh
# apx installer script
# Usage: curl -fsSL https://databricks-solutions.github.io/apx/install.sh | sh
#
# Options:
#   --version <tag>     Install a specific version (default: latest)
#   --no-modify-path    Don't modify shell profile to add to PATH
#   --install-dir <dir> Override the installation directory
#   --skill             Install Claude Code skill files only (no binary)
#   --global            With --skill, install to ~/.claude/ instead of project

set -eu

REPO="databricks-solutions/apx"
GITHUB_API="https://api.github.com"
GITHUB_RELEASES="https://github.com/${REPO}/releases/download"

# ---------------------------------------------------------------------------
# TTY-aware colors
# ---------------------------------------------------------------------------
if [ -t 1 ]; then
    GREEN='\033[0;32m'
    YELLOW='\033[0;33m'
    RED='\033[0;31m'
    CYAN='\033[0;36m'
    BOLD='\033[1m'
    RESET='\033[0m'
else
    GREEN=''
    YELLOW=''
    RED=''
    CYAN=''
    BOLD=''
    RESET=''
fi

info() {
    printf "${CYAN}info${RESET}: %s\n" "$@"
}

warn() {
    printf "${YELLOW}warn${RESET}: %s\n" "$@" >&2
}

error() {
    printf "${RED}error${RESET}: %s\n" "$@" >&2
}

success() {
    printf "${GREEN}success${RESET}: %s\n" "$@"
}

# ---------------------------------------------------------------------------
# Argument parsing
# ---------------------------------------------------------------------------
VERSION=""
NO_MODIFY_PATH=0
INSTALL_DIR=""
SKILL_ONLY=0
GLOBAL=0

while [ $# -gt 0 ]; do
    case "$1" in
        --version)
            shift
            VERSION="$1"
            ;;
        --no-modify-path)
            NO_MODIFY_PATH=1
            ;;
        --install-dir)
            shift
            INSTALL_DIR="$1"
            ;;
        --skill)
            SKILL_ONLY=1
            ;;
        --global)
            GLOBAL=1
            ;;
        -h|--help)
            printf "Usage: install.sh [OPTIONS]\n\n"
            printf "Options:\n"
            printf "  --version <tag>     Install a specific version (default: latest)\n"
            printf "  --no-modify-path    Don't modify shell profile\n"
            printf "  --install-dir <dir> Override installation directory\n"
            printf "  --skill             Install Claude Code skill files only (no binary)\n"
            printf "  --global            With --skill, install to ~/.claude/ instead of project\n"
            printf "  -h, --help          Show this help\n"
            exit 0
            ;;
        *)
            error "Unknown option: $1"
            exit 1
            ;;
    esac
    shift
done

# ---------------------------------------------------------------------------
# Platform detection
# ---------------------------------------------------------------------------
detect_platform() {
    OS="$(uname -s)"
    case "$OS" in
        Linux)  PLATFORM="linux" ;;
        Darwin) PLATFORM="darwin" ;;
        *)
            error "Unsupported operating system: $OS"
            exit 1
            ;;
    esac

    MACHINE="$(uname -m)"
    case "$MACHINE" in
        x86_64|amd64) ARCH="x86_64" ;;
        aarch64|arm64) ARCH="aarch64" ;;
        *)
            error "Unsupported architecture: $MACHINE"
            exit 1
            ;;
    esac

    info "Detected platform: ${PLATFORM} ${ARCH}"
}

# ---------------------------------------------------------------------------
# Check for existing installation
# ---------------------------------------------------------------------------
check_existing() {
    if command -v apx >/dev/null 2>&1; then
        EXISTING_VERSION="$(apx --version 2>/dev/null || true)"
        warn "apx is already installed: ${EXISTING_VERSION}"
        warn "Run 'apx upgrade' to update, or remove the existing installation first."
        exit 0
    fi
}

# ---------------------------------------------------------------------------
# Determine install directory
# ---------------------------------------------------------------------------
determine_install_dir() {
    if [ -n "$INSTALL_DIR" ]; then
        return
    fi

    if [ -n "${APX_INSTALL_DIR:-}" ]; then
        INSTALL_DIR="$APX_INSTALL_DIR"
    elif [ -n "${XDG_BIN_HOME:-}" ]; then
        INSTALL_DIR="$XDG_BIN_HOME"
    elif [ -n "${XDG_DATA_HOME:-}" ]; then
        INSTALL_DIR="$(dirname "$XDG_DATA_HOME")/bin"
    else
        INSTALL_DIR="$HOME/.local/bin"
    fi

    info "Install directory: ${INSTALL_DIR}"
}

# ---------------------------------------------------------------------------
# Resolve version
# ---------------------------------------------------------------------------
resolve_version() {
    if [ -n "$VERSION" ]; then
        info "Using specified version: ${VERSION}"
        return
    fi

    info "Fetching latest release..."

    if ! command -v curl >/dev/null 2>&1; then
        error "curl is required but not found"
        exit 1
    fi

    LATEST_URL="${GITHUB_API}/repos/${REPO}/releases/latest"
    RESPONSE="$(curl -fsSL "$LATEST_URL" 2>/dev/null)" || {
        error "Failed to fetch latest release from GitHub"
        exit 1
    }

    # Extract tag_name using sed (POSIX compatible)
    VERSION="$(printf '%s' "$RESPONSE" | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -1)"

    if [ -z "$VERSION" ]; then
        error "Could not determine latest version"
        exit 1
    fi

    info "Latest version: ${VERSION}"
}

# ---------------------------------------------------------------------------
# Download and install
# ---------------------------------------------------------------------------
download_and_install() {
    ASSET_NAME="apx-${ARCH}-${PLATFORM}"
    URL="${GITHUB_RELEASES}/${VERSION}/${ASSET_NAME}"

    info "Downloading ${URL}..."

    TMPDIR_INSTALL="$(mktemp -d)"
    trap 'rm -rf "$TMPDIR_INSTALL"' EXIT

    HTTP_CODE="$(curl -fsSL -w '%{http_code}' -o "${TMPDIR_INSTALL}/apx" "$URL" 2>/dev/null)" || {
        error "Download failed. Check that version '${VERSION}' exists and has a binary for ${PLATFORM} ${ARCH}."
        exit 1
    }

    if [ "$HTTP_CODE" != "200" ]; then
        error "Download returned HTTP ${HTTP_CODE}"
        exit 1
    fi

    chmod +x "${TMPDIR_INSTALL}/apx"

    mkdir -p "$INSTALL_DIR"
    mv "${TMPDIR_INSTALL}/apx" "${INSTALL_DIR}/apx"

    success "Installed apx to ${INSTALL_DIR}/apx"
}

# ---------------------------------------------------------------------------
# PATH modification
# ---------------------------------------------------------------------------
modify_path() {
    if [ "$NO_MODIFY_PATH" = 1 ]; then
        return
    fi

    # CI environment (GitHub Actions)
    if [ -n "${GITHUB_PATH:-}" ]; then
        printf '%s\n' "$INSTALL_DIR" >> "$GITHUB_PATH"
        info "Added ${INSTALL_DIR} to \$GITHUB_PATH"
        return
    fi

    # Check if already in PATH
    case ":${PATH}:" in
        *":${INSTALL_DIR}:"*) return ;;
    esac

    SHELL_NAME="$(basename "${SHELL:-/bin/sh}")"
    EXPORT_LINE="export PATH=\"${INSTALL_DIR}:\$PATH\""

    case "$SHELL_NAME" in
        bash)
            RC_FILE="$HOME/.bashrc"
            ;;
        zsh)
            RC_FILE="$HOME/.zshrc"
            ;;
        fish)
            RC_FILE="$HOME/.config/fish/config.fish"
            EXPORT_LINE="fish_add_path ${INSTALL_DIR}"
            ;;
        *)
            warn "Unknown shell: ${SHELL_NAME}. Add ${INSTALL_DIR} to your PATH manually."
            return
            ;;
    esac

    # Guard against duplicate entries
    if [ -f "$RC_FILE" ] && grep -qF "$INSTALL_DIR" "$RC_FILE" 2>/dev/null; then
        return
    fi

    printf '\n# Added by apx installer\n%s\n' "$EXPORT_LINE" >> "$RC_FILE"
    info "Added ${INSTALL_DIR} to PATH in ${RC_FILE}"
    warn "Restart your shell or run: ${EXPORT_LINE}"
}

# ---------------------------------------------------------------------------
# Dependency checks
# ---------------------------------------------------------------------------
check_dependencies() {
    if ! command -v uv >/dev/null 2>&1; then
        info "uv not found on PATH. apx will download it automatically on first use."
    fi

    if ! command -v databricks >/dev/null 2>&1; then
        warn "Databricks CLI is not installed. Some apx features require it."
        warn "Install it from: https://docs.databricks.com/aws/en/dev-tools/cli/install"
    fi
}

# ---------------------------------------------------------------------------
# Success banner
# ---------------------------------------------------------------------------
print_banner() {
    printf "\n"
    printf "${BOLD}${GREEN}apx ${VERSION} installed successfully!${RESET}\n"
    printf "\n"
    printf "  ${CYAN}Binary:${RESET}  ${INSTALL_DIR}/apx\n"
    printf "\n"
    printf "  Get started:\n"
    printf "    ${BOLD}apx init${RESET}       Create a new project\n"
    printf "    ${BOLD}apx dev start${RESET}  Start development server\n"
    printf "\n"
}

# ---------------------------------------------------------------------------
# Skill-only install
# ---------------------------------------------------------------------------
install_skill() {
    BRANCH="main"
    BASE_URL="https://raw.githubusercontent.com/${REPO}/${BRANCH}"

    SKILL_FILES="skills/apx/SKILL.md skills/apx/backend-patterns.md skills/apx/frontend-patterns.md"

    if [ "$GLOBAL" = 1 ]; then
        SKILL_DIR="$HOME/.claude/skills/apx"
        MCP_DIR="$HOME/.claude"
        info "Installing apx skill globally to ${SKILL_DIR}/"
    else
        SKILL_DIR=".claude/skills/apx"
        MCP_DIR="."
        info "Installing apx skill to ${SKILL_DIR}/ (project-level)"
    fi

    mkdir -p "$SKILL_DIR"

    FAILED=0
    for file in $SKILL_FILES; do
        filename="$(basename "$file")"
        info "Downloading ${filename}..."
        if ! curl -fsSL "${BASE_URL}/${file}" -o "${SKILL_DIR}/${filename}"; then
            error "Failed to download ${filename}"
            FAILED=1
        fi
    done

    # Download .mcp.json
    info "Downloading .mcp.json..."
    if ! curl -fsSL "${BASE_URL}/.mcp.json" -o "${MCP_DIR}/.mcp.json"; then
        error "Failed to download .mcp.json"
        FAILED=1
    fi

    if [ "$FAILED" -ne 0 ]; then
        error "Some files failed to download. Check your network connection and try again."
        exit 1
    fi

    printf "\n"
    success "apx skill installed!"
    printf "\n"
    printf "  Skill files:\n"
    for file in $SKILL_FILES; do
        filename="$(basename "$file")"
        printf "    %s\n" "${SKILL_DIR}/${filename}"
    done
    printf "  MCP config:  %s\n" "${MCP_DIR}/.mcp.json"
    printf "\n"

    if [ "$GLOBAL" = 0 ]; then
        info "Tip: Use --global to install for all projects instead."
    fi
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------
main() {
    if [ "$SKILL_ONLY" = 1 ]; then
        install_skill
        return
    fi

    detect_platform
    check_existing
    determine_install_dir
    resolve_version
    download_and_install
    modify_path
    check_dependencies
    print_banner
}

main
