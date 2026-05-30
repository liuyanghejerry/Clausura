# Release Clausura Skill

Trigger phrase: `release`, `发包`, `bump version`, `发布`

## Release Pipeline

```
git tag vX.Y.Z → push
  → Release workflow triggered (release.yml)
    ├── build (4 targets: x86_64/aarch64 linux + macos)
    ├── release (GitHub Release + artifacts)
    ├── docker (ghcr.io)
    └── publish (crates.io: core → sleep 30s → cli)
```

## How to Release

### 1. Bump versions

Update these files:
- `crates/clausura-core/Cargo.toml`: `version = "X.Y.Z"`
- `crates/clausura-cli/Cargo.toml`: `version = "X.Y.Z"` + `clausura-core = { version = "X.Y.Z" }`

### 2. Verify

```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
```

### 3. Commit & Tag

```bash
git add crates/*/Cargo.toml
git commit -m "release: bump to X.Y.Z"
git tag -a vX.Y.Z -m "vX.Y.Z - <summary>"
git push origin main
git push origin vX.Y.Z
```

### 4. Monitor

Watch: https://github.com/liuyanghejerry/Clausura/actions/workflows/release.yml

Expected: ~4 minutes, 7 jobs all green.

### 5. Verify publications

```bash
gh release view vX.Y.Z
curl -s https://crates.io/api/v1/crates/clausura-cli | jq '.crate.max_stable_version'
```

## Outputs

| Artifact | Location |
|----------|----------|
| 4 binaries (tar.gz) | GitHub Release attachments |
| Docker image | `ghcr.io/liuyanghejerry/clausura:vX.Y.Z` + `:latest` |
| `clausura-core` | https://crates.io/crates/clausura-core |
| `clausura-cli` | https://crates.io/crates/clausura-cli |
| `cargo install` | `cargo install clausura-cli` (auto-fetches latest) |

## Known Gotchas

### crates.io publish fails on clausura-cli

**Symptom**: `cargo publish -p clausura-cli` fails with exit code 101

**Cause**: `clausura-cli/Cargo.toml` has `clausura-core = { path = "..." }` without explicit `version` field.

**Fix**: Ensure dependency has `version = "X.Y.Z"` matching the published crate.

### crates.io shows "no README.md"

**Fix**: Ensure both `Cargo.toml` files have `readme = "../../README.md"`.

### CI formatting failure

**Symptom**: `cargo fmt --all -- --check` exits 1 in CI.

**Fix**: Run `cargo fmt --all` locally before committing. Pre-commit hook catches this: `git config core.hooksPath .githooks`.

### aarch64-linux build fails

**Symptom**: Linker errors in CI.

**Fix**: Already handled — `release.yml` installs `gcc-aarch64-linux-gnu` and sets `CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER`.

### Docker build fails

**Symptom**: `build.rs` fails in Docker stage.

**Fix**: Ensure `Dockerfile` installs `git` (`apk add --no-cache musl-dev git`).

## Release Checklist

- [ ] All tests pass: `cargo test --workspace`
- [ ] Clippy clean: `cargo clippy --workspace -- -D warnings`
- [ ] Formatted: `cargo fmt --all -- --check`
- [ ] Versions bumped in both `Cargo.toml` files + dependency version
- [ ] `readme = "../../README.md"` present in both `Cargo.toml`
- [ ] CI green: https://github.com/liuyanghejerry/Clausura/actions
- [ ] crates.io: both crates show correct version + README renders
- [ ] GitHub Release: binaries attached, release notes generated
- [ ] Docker: `docker pull ghcr.io/liuyanghejerry/clausura:latest` works
