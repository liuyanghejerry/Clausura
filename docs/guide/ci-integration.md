# CI Integration

Clausura auto-detects your CI environment and integrates with GitHub Actions, GitLab CI, Jenkins, and generic CI systems.

## How Detection Works

Clausura checks well-known environment variables in this order:

1. `GITHUB_ACTIONS` → GitHub Actions
2. `GITLAB_CI` → GitLab CI
3. `JENKINS_URL` → Jenkins
4. `CI=true` or `CI=1` → Generic CI
5. None of the above → Local (no CI context)

When detected, Clausura gathers repository, PR number, commit SHA, and branch context. These are available as template variables in `prompt_template` and embedded in SARIF output.

## GitHub Actions

### Option A: Composite Action (Simplest)

```yaml
name: Code Review
on: [pull_request]

jobs:
  review:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 2          # Required for git diff

      - uses: liuyanghejerry/Clausura@v1
        with:
          config: .clausura.yaml
          api_key: ${{ secrets.LLM_API_KEY }}
          model: gpt-4o           # Optional overrides
          vendor: openai
          token_budget: 32000
          timeout: 300
          version: latest         # Or pin: "1.2.1"
```

The composite action:
1. Downloads the matching release binary for the runner's OS/arch
2. Verifies the binary against the release's SHA256 checksums
3. Runs `clausura run` with your config

### Option B: Direct Binary

```yaml
name: Code Review
on: [pull_request]

jobs:
  review:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 2

      - name: Install Clausura
        run: |
          curl -fsSL https://raw.githubusercontent.com/liuyanghejerry/Clausura/main/install.sh | bash

      - name: Run Clausura
        run: clausura run
        env:
          CLAUSURA_API_KEY: ${{ secrets.LLM_API_KEY }}

      - name: Upload SARIF
        if: always()
        uses: github/codeql-action/upload-sarif@v3
        with:
          sarif_file: clausura-output.sarif
```

Uploading SARIF to GitHub integrates findings directly into the PR diff view and the Security tab.

### Option C: Docker

```yaml
- name: Run Clausura
  run: |
    docker run --rm \
      -v ${{ github.workspace }}:/workspace \
      -e CLAUSURA_API_KEY=${{ secrets.LLM_API_KEY }} \
      ghcr.io/liuyanghejerry/clausura:latest run
```

### Branch Protection

After setting up the workflow, configure branch protection rules to require the `review` job before merging:

1. Go to **Settings → Branches → Branch protection rules**
2. Add a rule for your protected branch (e.g., `main`)
3. Check **Require status checks to pass before merging**
4. Search for and select the `review` job
5. Save

Now PRs can't be merged unless Clausura passes.

## GitLab CI

```yaml
clausura-review:
  image: ghcr.io/liuyanghejerry/clausura:latest
  stage: review
  script:
    - clausura run
  variables:
    CLAUSURA_API_KEY: $LLM_API_KEY
    CLAUSURA_MODEL: "gpt-4o"
  artifacts:
    when: always
    paths:
      - clausura-output.sarif
    expire_in: 30 days
  rules:
    - if: $CI_PIPELINE_SOURCE == "merge_request_event"
```

Or using the install script:

```yaml
clausura-review:
  stage: review
  script:
    - curl -fsSL https://raw.githubusercontent.com/liuyanghejerry/Clausura/main/install.sh | bash
    - clausura run
  variables:
    CLAUSURA_API_KEY: $LLM_API_KEY
  rules:
    - if: $CI_PIPELINE_SOURCE == "merge_request_event"
```

## Jenkins

### Pipeline (Declarative)

```groovy
pipeline {
    agent any

    environment {
        CLAUSURA_API_KEY = credentials('llm-api-key')
    }

    stages {
        stage('Code Review') {
            steps {
                sh '''
                    curl -fsSL https://raw.githubusercontent.com/liuyanghejerry/Clausura/main/install.sh | bash
                    clausura run --model gpt-4o
                '''
            }
        }
    }

    post {
        always {
            archiveArtifacts artifacts: 'clausura-output.sarif', fingerprint: true
        }
    }
}
```

### GitHub Branch Source / Multibranch Pipeline

Clausura auto-detects the PR context from Jenkins environment variables when using the GitHub Branch Source plugin.

## Generic CI

Any CI system that sets `CI=true` is detected. Set these environment variables for context information:

```bash
export CI=true
export CI_REPO="owner/repo"
export CI_PR_NUMBER="42"
export CI_COMMIT_SHA="abc123def456"
export CI_BRANCH="feature/new-login"

export CLAUSURA_API_KEY=sk-...
clausura run
```

| Variable | Purpose | Required |
|----------|---------|----------|
| `CI` | Must be `true` or `1` for Clausura to detect CI mode | Yes |
| `CI_REPO` | Repository name (appears in SARIF) | No |
| `CI_PR_NUMBER` | Pull request number (appears in SARIF) | No |
| `CI_COMMIT_SHA` | Current commit SHA (appears in SARIF) | No |
| `CI_BRANCH` | Current branch name (appears in SARIF) | No |

## Template Variables in CI

When CI context is detected, these template variables are available in `prompt_template`:

```yaml
prompt_template: |
  Repository: {{repo}}
  Branch: {{branch}}
  Commit: {{commit_sha}}
  PR: {{pr_number}}
  Platform: {{ci_platform}}

  Review the diff for security issues...
```

| Variable | Source |
|----------|--------|
| `{{repo}}` | Repo name from CI context |
| `{{branch}}` | Current branch |
| `{{commit_sha}}` | Current commit |
| `{{pr_number}}` | PR number |
| `{{ci_platform}}` | `github_actions`, `gitlab_ci`, `jenkins`, `generic_ci`, or `local` |

## SARIF Upload

Clausura always writes `clausura-output.sarif`. In CI, upload it to your platform's security dashboard:

### GitHub Advanced Security

```yaml
- uses: github/codeql-action/upload-sarif@v3
  if: always()
  with:
    sarif_file: clausura-output.sarif
```

### GitLab

SARIF files can be uploaded as pipeline artifacts and viewed with GitLab's SARIF support (GitLab Ultimate).

### Generic

SARIF is an open standard. View it with any SARIF viewer (VS Code extension, standalone tools) or parse it as JSON for custom dashboards.

## Parallel Jobs

For multi-dimensional review, run separate Clausura tasks in parallel CI jobs:

```yaml
# GitHub Actions
jobs:
  security:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with: { fetch-depth: 2 }
      - uses: liuyanghejerry/Clausura@v1
        with:
          config: .clausura/security.yaml
          api_key: ${{ secrets.LLM_API_KEY }}

  i18n:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with: { fetch-depth: 2 }
      - uses: liuyanghejerry/Clausura@v1
        with:
          config: .clausura/i18n.yaml
          api_key: ${{ secrets.LLM_API_KEY }}
```

Each job independently passes or fails. Use branch protection rules to require all review jobs.

## Checkpoint Persistence

Checkpoints are stored in `~/.clausura/checkpoints.db` (user home directory). In ephemeral CI containers without a persistent home volume, checkpoints do not survive between runs — `--resume` will have nothing to restore from.

For persistent checkpointing in CI, mount a volume at `$HOME/.clausura/`.

## Best Practices

1. **`fetch-depth: 2`** — Clausura's `git_diff` tool needs the previous commit for comparison. Always set `fetch-depth: 2` (or higher) in your checkout step.

2. **Use secrets for API keys** — Never commit API keys. Use your CI's secrets manager (`${{ secrets.LLM_API_KEY }}`, GitLab CI/CD variables, Jenkins credentials).

3. **Upload SARIF on failure** — Use `if: always()` so SARIF is available for debugging even when the pipeline fails.

4. **Set realistic timeouts** — The CI job timeout should exceed `task.timeout_secs` by a comfortable margin (add 60s for installation and SARIF writing).

5. **Run on PRs, not pushes to main** — Clausura compares against the base branch; running on pushes to `main` without a PR context may not produce meaningful diffs.

## Next

→ [Troubleshooting common issues](troubleshooting.md)
