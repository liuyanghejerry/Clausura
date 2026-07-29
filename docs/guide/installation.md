# Installation

## Supported Platforms

Clausura supports **Linux** and **macOS** (x86_64 and aarch64).

Windows is not directly supported — use WSL2 or Docker instead.

## Method 1: Install Script (Recommended)

The install script downloads the latest release binary for your OS/arch, verifies its SHA256 checksum, and installs to `/usr/local/bin` (or `~/.local/bin` if `/usr/local/bin` is not writable).

```bash
curl -fsSL https://raw.githubusercontent.com/liuyanghejerry/Clausura/main/install.sh | bash
```

To install a specific version:

```bash
curl -fsSL https://raw.githubusercontent.com/liuyanghejerry/Clausura/main/install.sh | bash -s -- v1.2.1
```

### What the script does

1. Detects your OS (Linux/macOS) and architecture (x86_64/aarch64)
2. Downloads the matching `.tar.gz` from GitHub Releases
3. Downloads `checksums.txt` from the same release
4. Verifies the archive's SHA256 hash
5. Extracts the binary and installs it

## Method 2: Cargo

Requires Rust toolchain (install via [rustup](https://rustup.rs)).

```bash
cargo install clausura-cli
```

This compiles from source and installs to `~/.cargo/bin/clausura`.

To install a specific version:

```bash
cargo install clausura-cli --version 1.2.1
```

## Method 3: Docker

Pre-built images are published to GitHub Container Registry:

```bash
docker pull ghcr.io/liuyanghejerry/clausura:latest

# Run with current directory mounted as workspace
docker run --rm -v $(pwd):/workspace ghcr.io/liuyanghejerry/clausura run

# With API key
docker run --rm \
  -v $(pwd):/workspace \
  -e CLAUSURA_API_KEY=sk-... \
  ghcr.io/liuyanghejerry/clausura run
```

Tag format: `latest` for the most recent release, `v1.2.1` for a specific version.

## Method 4: Build from Source

```bash
git clone https://github.com/liuyanghejerry/Clausura.git
cd Clausura
cargo build --release --package clausura-cli
# Binary at ./target/release/clausura
```

## Verify Installation

```bash
clausura --version
# Example output:
# clausura 1.2.1 (commit: a1b2c3d, built: 2026-07-15)
```

## GitHub Actions

If you're using GitHub Actions, the simplest approach is the composite action — it handles installation automatically:

```yaml
- uses: liuyanghejerry/Clausura@v1
  with:
    config: .clausura.yaml
    api_key: ${{ secrets.LLM_API_KEY }}
```

The action downloads the correct binary for the runner, verifies it, and runs Clausura with your config. See [CI Integration](ci-integration.md) for details.

## Upgrading

For script-based installs, re-run the install script.

For Cargo:

```bash
cargo install clausura-cli --force
```

For Docker, pull the new tag:

```bash
docker pull ghcr.io/liuyanghejerry/clausura:latest
```

## Uninstalling

```bash
# Script / Cargo install
rm $(which clausura)

# Also remove config and checkpoints
rm -rf ~/.clausura
```

## Next

→ [Set up your LLM provider](llm-providers.md)
