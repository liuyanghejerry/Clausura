#!/usr/bin/env bash
set -euo pipefail

APP_NAME="clausura"
APP_VERSION="${1:-latest}"
GITHUB_REPO="liuyanghejerry/Clausura"

# --- Color helpers ---
RED='\033[0;31m'
YELLOW='\033[1;33m'
GREEN='\033[0;32m'
NC='\033[0m' # No Color

info()  { echo -e "${GREEN}[info]${NC} $*"; }
warn()  { echo -e "${YELLOW}[warn]${NC} $*"; }
error() { echo -e "${RED}[error]${NC} $*"; }

# --- Detect OS and arch ---
OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH="$(uname -m)"

case "$OS" in
    linux)   TARGET="unknown-linux-gnu" ;;
    darwin)  TARGET="apple-darwin" ;;
    mingw*|msys*|cygwin*) OS="windows"; TARGET="pc-windows-msvc" ;;
    *)       error "Unsupported OS: $OS"; exit 1 ;;
esac

case "$ARCH" in
    x86_64|amd64) ARCH="x86_64" ;;
    aarch64|arm64) ARCH="aarch64" ;;
    *)           error "Unsupported arch: $ARCH"; exit 1 ;;
esac

# --- Determine install directory ---
if [ -w "/usr/local/bin" ]; then
    INSTALL_DIR="/usr/local/bin"
elif [ -w "$HOME/.local/bin" ]; then
    INSTALL_DIR="$HOME/.local/bin"
else
    mkdir -p "$HOME/.local/bin"
    INSTALL_DIR="$HOME/.local/bin"
fi

mkdir -p "$INSTALL_DIR"

# --- Construct download URL ---
if [ "$APP_VERSION" = "latest" ]; then
    DOWNLOAD_URL="https://github.com/$GITHUB_REPO/releases/latest/download/${APP_NAME}-${ARCH}-${TARGET}.tar.gz"
else
    DOWNLOAD_URL="https://github.com/$GITHUB_REPO/releases/download/v${APP_VERSION}/${APP_NAME}-${ARCH}-${TARGET}.tar.gz"
fi

# --- Try prebuilt binary first ---
download_binary() {
    info "Downloading $APP_NAME $APP_VERSION for ${ARCH}-${TARGET}..."
    local tmp_dir
    tmp_dir=$(mktemp -d)
    trap "rm -rf '$tmp_dir'" EXIT

    local http_code
    if command -v curl &>/dev/null; then
        http_code=$(curl -fsSL -w "%{http_code}" -o "$tmp_dir/${APP_NAME}.tar.gz" "$DOWNLOAD_URL")
    elif command -v wget &>/dev/null; then
        # wget: capture HTTP status from server response headers
        http_code=$(wget -q --server-response "$DOWNLOAD_URL" -O "$tmp_dir/${APP_NAME}.tar.gz" 2>&1 | \
            awk '/HTTP\// {print $2}' | tail -1)
        if [ -z "$http_code" ]; then
            http_code="000"
        fi
    else
        error "curl or wget is required for download"
        return 1
    fi

    if [ "$http_code" != "200" ]; then
        warn "Prebuilt binary not available (HTTP $http_code)"
        return 1
    fi

    tar xzf "$tmp_dir/${APP_NAME}.tar.gz" -C "$tmp_dir"
    cp "$tmp_dir/${APP_NAME}" "$INSTALL_DIR/"
    chmod +x "$INSTALL_DIR/$APP_NAME"
    return 0
}

# --- Fallback: install via cargo ---
install_via_cargo() {
    if ! command -v cargo &>/dev/null; then
        error "Prebuilt binary not available and 'cargo' is not installed."
        echo ""
        echo "Install options:"
        echo "  1. Install Rust:  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
        echo "     Then run:        cargo install clausura-cli"
        echo "  2. Use Docker:     docker pull ghcr.io/liuyanghejerry/clausura:latest"
        echo "  3. Build from source:"
        echo "     git clone https://github.com/liuyanghejerry/Clausura.git"
        echo "     cd Clausura && cargo build --release --package clausura-cli"
        return 1
    fi

    warn "Prebuilt binary not available, falling back to cargo install..."
    if [ -n "${APP_VERSION}" ] && [ "$APP_VERSION" != "latest" ]; then
        cargo install clausura-cli --version "$APP_VERSION"
    else
        cargo install clausura-cli
    fi

    # Find where cargo installed the binary
    if command -v "$APP_NAME" &>/dev/null; then
        INSTALL_DIR="$(dirname "$(command -v "$APP_NAME")")"
    fi
    return 0
}

# --- Main ---
if download_binary; then
    info "Installed $APP_NAME (prebuilt) to $INSTALL_DIR/$APP_NAME"
elif install_via_cargo; then
    info "Installed $APP_NAME (via cargo) to $INSTALL_DIR/$APP_NAME"
else
    error "Installation failed."
    exit 1
fi

# --- Verify ---
if command -v "$APP_NAME" &>/dev/null; then
    echo ""
    info "Installation successful!"
    "$APP_NAME" --version 2>/dev/null || true
    echo "Run '$APP_NAME --help' to get started."
else
    warn "$APP_NAME was installed but is not on PATH."
    echo "Add $INSTALL_DIR to your PATH:"
    echo "  export PATH=\"$INSTALL_DIR:\$PATH\""
fi
