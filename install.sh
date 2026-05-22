#!/usr/bin/env bash
set -euo pipefail

APP_NAME="clausura"
APP_VERSION="${1:-latest}"
GITHUB_REPO="clausura/clausura"

# Detect OS and arch
OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH="$(uname -m)"

case "$OS" in
    linux)   TARGET="unknown-linux-gnu" ;;
    darwin)  TARGET="apple-darwin" ;;
    mingw*|msys*|cygwin*) OS="windows"; TARGET="pc-windows-msvc" ;;
    *)       echo "Unsupported OS: $OS"; exit 1 ;;
esac

case "$ARCH" in
    x86_64|amd64) ARCH="x86_64" ;;
    aarch64|arm64) ARCH="aarch64" ;;
    *)           echo "Unsupported arch: $ARCH"; exit 1 ;;
esac

# Determine install directory
if [ -w "/usr/local/bin" ]; then
    INSTALL_DIR="/usr/local/bin"
elif [ -w "$HOME/.local/bin" ]; then
    INSTALL_DIR="$HOME/.local/bin"
else
    mkdir -p "$HOME/.local/bin"
    INSTALL_DIR="$HOME/.local/bin"
fi

mkdir -p "$INSTALL_DIR"

# Download URL
if [ "$APP_VERSION" = "latest" ]; then
    DOWNLOAD_URL="https://github.com/$GITHUB_REPO/releases/latest/download/${APP_NAME}-${ARCH}-${TARGET}.tar.gz"
else
    DOWNLOAD_URL="https://github.com/$GITHUB_REPO/releases/download/v${APP_VERSION}/${APP_NAME}-${ARCH}-${TARGET}.tar.gz"
fi

echo "Downloading $APP_NAME $APP_VERSION for ${ARCH}-${TARGET}..."
TMP_DIR=$(mktemp -d)
cd "$TMP_DIR"

if command -v curl &>/dev/null; then
    curl -fsSL "$DOWNLOAD_URL" -o "${APP_NAME}.tar.gz"
elif command -v wget &>/dev/null; then
    wget -q "$DOWNLOAD_URL" -O "${APP_NAME}.tar.gz"
else
    echo "Error: curl or wget required"
    exit 1
fi

tar xzf "${APP_NAME}.tar.gz"
cp "${APP_NAME}" "$INSTALL_DIR/"
chmod +x "$INSTALL_DIR/$APP_NAME"
rm -rf "$TMP_DIR"

echo "Installed $APP_NAME to $INSTALL_DIR/$APP_NAME"
echo "Run '$APP_NAME --version' to verify"
