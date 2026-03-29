#!/usr/bin/env bash
# ============================================================================
# SquadyAI Realtime API — One-line Installer
# ============================================================================
#
#   curl -fsSL https://raw.githubusercontent.com/SquadyAI/RealtimeAPI/main/server/install.sh | bash
#
# What it does:
#   1. Detects OS and architecture
#   2. Downloads the latest release tarball from GitHub
#   3. Extracts to ~/.realtime/
#   4. Creates /usr/local/bin/realtime symlink
#
# To uninstall:
#   realtime uninstall && rm -rf ~/.realtime
# ============================================================================

set -euo pipefail

REPO="SquadyAI/RealtimeAPI"
INSTALL_DIR="${HOME}/.realtime"
BIN_DIR="/usr/local/bin"
BIN_NAME="realtime"

# ── Colors ────────────────────────────────────────────────────────────────

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
DIM='\033[2m'
NC='\033[0m'

info()  { echo -e "  ${GREEN}✓${NC} $1"; }
warn()  { echo -e "  ${YELLOW}⚠${NC} $1"; }
err()   { echo -e "  ${RED}✗${NC} $1"; exit 1; }

# ── Detect platform ──────────────────────────────────────────────────────

detect_platform() {
    local os arch

    case "$(uname -s)" in
        Linux*)  os="linux" ;;
        Darwin*) os="darwin" ;;
        *)       err "Unsupported OS: $(uname -s). Only Linux and macOS are supported." ;;
    esac

    case "$(uname -m)" in
        x86_64|amd64)   arch="x86_64" ;;
        arm64|aarch64)  arch="aarch64" ;;
        *)              err "Unsupported architecture: $(uname -m). Only x86_64 and arm64 are supported." ;;
    esac

    echo "${os}-${arch}"
}

# ── Find latest release ──────────────────────────────────────────────────

get_latest_version() {
    local url="https://api.github.com/repos/${REPO}/releases/latest"
    local version

    if command -v curl &>/dev/null; then
        version=$(curl -fsSL "$url" 2>/dev/null | grep '"tag_name"' | head -1 | sed 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/')
    elif command -v wget &>/dev/null; then
        version=$(wget -qO- "$url" 2>/dev/null | grep '"tag_name"' | head -1 | sed 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/')
    else
        err "Neither curl nor wget found. Please install one of them."
    fi

    if [[ -z "$version" ]]; then
        err "Could not determine latest version. Check your internet connection or visit:\n  https://github.com/${REPO}/releases"
    fi

    echo "$version"
}

# ── Download and install ─────────────────────────────────────────────────

download_and_install() {
    local version="$1"
    local platform="$2"

    # Strip leading 'v' for filename
    local ver_num="${version#v}"
    local filename="realtime-${ver_num}-${platform}.tar.gz"
    local download_url="https://github.com/${REPO}/releases/download/${version}/${filename}"
    local tmp_dir
    tmp_dir="$(mktemp -d)"

    echo ""
    echo -e "  ${CYAN}Downloading ${BOLD}realtime ${version}${NC}${CYAN} for ${platform}...${NC}"
    echo -e "  ${DIM}${download_url}${NC}"
    echo ""

    # Download
    if command -v curl &>/dev/null; then
        curl -fSL --progress-bar -o "${tmp_dir}/${filename}" "$download_url" || \
            err "Download failed. The release may not exist yet for your platform.\n  Check: https://github.com/${REPO}/releases"
    else
        wget -q --show-progress -O "${tmp_dir}/${filename}" "$download_url" || \
            err "Download failed."
    fi

    # Remove old installation
    if [[ -d "$INSTALL_DIR" ]]; then
        warn "Removing previous installation at ${INSTALL_DIR}"
        rm -rf "$INSTALL_DIR"
    fi

    # Extract
    mkdir -p "$INSTALL_DIR"
    tar -xzf "${tmp_dir}/${filename}" -C "$INSTALL_DIR"
    chmod +x "${INSTALL_DIR}/realtime"

    # Make Rust binary executable if present
    if [[ -f "${INSTALL_DIR}/realtime-server" ]]; then
        chmod +x "${INSTALL_DIR}/realtime-server"
    fi

    # Cleanup
    rm -rf "$tmp_dir"

    info "Installed to ${INSTALL_DIR}"
}

# ── Create symlink ────────────────────────────────────────────────────────

create_symlink() {
    local target="${BIN_DIR}/${BIN_NAME}"
    local src="${INSTALL_DIR}/realtime"

    if [[ -L "$target" ]]; then
        # Update existing symlink
        sudo rm "$target"
    elif [[ -f "$target" ]]; then
        warn "${target} exists and is not a symlink. Skipping."
        warn "Add ${INSTALL_DIR} to your PATH manually, or remove ${target} first."
        return
    fi

    # Try without sudo first (works if /usr/local/bin is writable)
    if ln -sf "$src" "$target" 2>/dev/null; then
        info "Linked: ${target} -> ${src}"
    else
        echo -e "  ${DIM}Need sudo to create ${target}${NC}"
        sudo ln -sf "$src" "$target"
        info "Linked: ${target} -> ${src}"
    fi
}

# ── Main ──────────────────────────────────────────────────────────────────

main() {
    echo ""
    echo -e "  ${CYAN}${BOLD}SquadyAI Realtime API — Installer${NC}"
    echo ""

    # Check dependencies
    if ! command -v tar &>/dev/null; then
        err "'tar' is required but not found."
    fi

    local platform version
    platform="$(detect_platform)"
    info "Platform: ${platform}"

    version="$(get_latest_version)"
    info "Latest version: ${version}"

    download_and_install "$version" "$platform"
    create_symlink

    echo ""
    echo -e "  ${GREEN}${BOLD}Installation complete!${NC}"
    echo ""
    echo -e "  Get started:"
    echo -e "    ${BOLD}cd your-project${NC}"
    echo -e "    ${BOLD}realtime onboard${NC}    # interactive setup"
    echo -e "    ${BOLD}realtime onboard${NC}    # start the service"
    echo ""
    echo -e "  Other commands:"
    echo -e "    realtime doctor     # diagnose issues"
    echo -e "    realtime version    # show version"
    echo -e "    realtime help       # full help"
    echo ""
    echo -e "  To uninstall:"
    echo -e "    ${DIM}realtime uninstall && rm -rf ~/.realtime${NC}"
    echo ""
}

main "$@"
