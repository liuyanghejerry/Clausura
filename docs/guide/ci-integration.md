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

### Complete PR workflow

Copy [`examples/github-actions-review.yml`](../../examples/github-actions-review.yml)
to `.github/workflows/clausura.yml`. The updated Action and `--base` CLI option
require a release containing these changes (after v1.7.0); until released,
build this branch and use the direct-binary steps below.

```yaml
# Copy to .github/workflows/clausura.yml in the repository to review.
# Use an Action ref and binary release containing --base (after v1.7.0).
name: Clausura Review
on: [pull_request]
permissions:
  contents: read
jobs:
  review:
    # Fork PRs and Dependabot do not receive the model secret in this workflow.
    if: github.event.pull_request.head.repo.full_name == github.repository && github.actor != 'dependabot[bot]'
    runs-on: ubuntu-latest
    timeout-minutes: 15
    steps:
      - uses: actions/checkout@v7
        with:
          ref: ${{ github.event.pull_request.head.sha }}
          fetch-depth: 0
          persist-credentials: false

      # Install configured MCP servers / language servers here, before review.
      - name: Review committed PR changes
        id: clausura
        uses: liuyanghejerry/Clausura@v1
        with:
          config: .clausura.yaml
          api_key: ${{ secrets.LLM_API_KEY }}
          base: ${{ github.event.pull_request.base.sha }}
```

The checkout uses the PR head commit, with full history to resolve the common
ancestor with the PR base. `fetch-depth: 2` is not sufficient for arbitrary
multi-commit or diverged PRs. The Action defaults `base` to the PR's base SHA;
an explicit `base` input overrides it. It does not fetch or change your checkout.
An unavailable base/history fails with exit 2 before calling the model.

The Action downloads and verifies the release binary, runs the review, writes a
job summary, uploads reports, then applies the original exit code. Artifacts
include SARIF, summary JSON (when produced), the execution log and `exit-code.txt`.
Setup/config errors can occur before SARIF/JSON exists. Installation failures are
reported in the installation step's log. Gate violations remain failures even
when report upload succeeds.

Use `artifact_name` to give each invocation a unique name in matrix jobs or when
running multiple reviews in one job. Set `upload_artifact: 'false'` to manage
artifacts yourself (for example on GitHub Enterprise Server). Outputs `exit_code`,
`sarif`, and `summary` are available to subsequent steps. Omitted model/vendor/
budget inputs preserve the caller's environment and YAML settings.

This example reviews same-repository PRs with a model secret. Fork and Dependabot
PRs are skipped because they do not receive that secret; a skipped job is not
proof that those changes were reviewed. Arrange a separate trusted review before
requiring coverage of those PRs. Do not switch to `pull_request_target` to expose
secrets to untrusted PR code.

### Direct binary (or build from source)

Build `cargo build --release --package clausura-cli` in a trusted Clausura source
checkout and put the resulting binary on the runner PATH. In the target repository,
use the same checkout and dependency ordering as above, replacing the Action with:

```yaml
- name: Run Clausura
  env:
    CLAUSURA_API_KEY: ${{ secrets.LLM_API_KEY }}
    REVIEW_BASE: ${{ github.event.pull_request.base.sha }}
  run: clausura run --base "$REVIEW_BASE" --summary clausura-summary.json

- name: Upload reports
  if: always()
  uses: actions/upload-artifact@v7
  with:
    name: clausura-review
    path: |
      clausura-output.sarif
      clausura-summary.json
    if-no-files-found: warn
```

After release, install a matching released binary instead of building from source.
For Docker, pass `--base` too, mount the checkout with its Git history, and forward
secrets by environment name rather than inserting their value into the command:

```yaml
- name: Review with Docker
  env:
    CLAUSURA_API_KEY: ${{ secrets.LLM_API_KEY }}
    REVIEW_BASE: ${{ github.event.pull_request.base.sha }}
  run: |
    docker run --rm -v "$GITHUB_WORKSPACE:/workspace" \
      -e CLAUSURA_API_KEY ghcr.io/liuyanghejerry/clausura:latest \
      run --base "$REVIEW_BASE" --summary /workspace/clausura-summary.json
```

### Optional GitHub code scanning

Artifact upload works without enabling code scanning. To additionally publish
SARIF to code scanning, enable it for the repository, add `security-events: write`
to job permissions, and place this step after the Action:

```yaml
- name: Publish SARIF to code scanning
  if: ${{ always() && steps.clausura.outputs.sarif_exists == 'true' }}
  uses: github/codeql-action/upload-sarif@v4
  with:
    sarif_file: ${{ steps.clausura.outputs.sarif }}
```

Availability depends on repository visibility and enabled GitHub security
features; see [GitHub's SARIF upload requirements](https://docs.github.com/en/code-security/how-tos/find-and-fix-code-vulnerabilities/integrate-with-existing-tools/upload-sarif-file).

### Branch Protection

After setting up the workflow, configure branch protection rules to require the `review` job before merging:

1. Go to **Settings → Branches → Branch protection rules**
2. Add a rule for your protected branch (e.g., `main`)
3. Check **Require status checks to pass before merging**
4. Search for and select the `review` job
5. Save

For jobs that execute, a gate violation or incomplete review blocks merging.
The fork/Dependabot skip policy above still needs a separate review policy.

## GitLab CI

```yaml
clausura-review:
  image: ghcr.io/liuyanghejerry/clausura:latest
  stage: review
  script:
    - clausura run --base "$CI_MERGE_REQUEST_DIFF_BASE_SHA" --summary clausura-summary.json
  variables:
    CLAUSURA_API_KEY: $LLM_API_KEY
    GIT_DEPTH: "0"
    CLAUSURA_MODEL: "gpt-4o"
  artifacts:
    when: always
    paths:
      - clausura-output.sarif
      - clausura-summary.json
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
    - clausura run --base "$CI_MERGE_REQUEST_DIFF_BASE_SHA" --summary clausura-summary.json
  variables:
    CLAUSURA_API_KEY: $LLM_API_KEY
    GIT_DEPTH: "0"
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
                    clausura run --model gpt-4o --base "origin/$CHANGE_TARGET" --summary clausura-summary.json
                '''
            }
        }
    }

    post {
        always {
            archiveArtifacts artifacts: 'clausura-output.sarif,clausura-summary.json', allowEmptyArchive: true, fingerprint: true
        }
    }
}
```

### GitHub Branch Source / Multibranch Pipeline

Clausura auto-detects PR metadata from Jenkins environment variables. Fetch the target branch and enough history for merge-base before review; this example assumes `origin/$CHANGE_TARGET` exists locally.

## Generic CI

Any CI system that sets `CI=true` is detected. Set these environment variables for context information:

```bash
export CI=true
export CI_REPO="owner/repo"
export CI_PR_NUMBER="42"
export CI_COMMIT_SHA="abc123def456"
export CI_BRANCH="feature/new-login"

export CLAUSURA_API_KEY=sk-...
clausura run --base origin/main --summary clausura-summary.json
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

Completed agent executions write SARIF; setup/config errors may not. Upload existing reports even when review fails:

### GitHub Advanced Security

```yaml
- uses: github/codeql-action/upload-sarif@v4
  if: ${{ always() && hashFiles('clausura-output.sarif') != '' }}
  with:
    sarif_file: clausura-output.sarif
```

### GitLab

Upload SARIF and summary JSON as pipeline artifacts. This does not automatically populate the GitLab security dashboard.

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
      - uses: actions/checkout@v7
        with:
          ref: ${{ github.event.pull_request.head.sha }}
          fetch-depth: 0
          persist-credentials: false
      - uses: liuyanghejerry/Clausura@v1
        with:
          config: .clausura/security.yaml
          api_key: ${{ secrets.LLM_API_KEY }}

  i18n:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v7
        with:
          ref: ${{ github.event.pull_request.head.sha }}
          fetch-depth: 0
          persist-credentials: false
      - uses: liuyanghejerry/Clausura@v1
        with:
          config: .clausura/i18n.yaml
          api_key: ${{ secrets.LLM_API_KEY }}
```

Use the same pull_request trigger, permissions and fork/Dependabot condition as
the complete workflow. Each job independently passes or fails; require the
appropriate jobs in branch protection.

## Checkpoint Persistence

Checkpoints are stored in `~/.clausura/checkpoints.db` (user home directory). In ephemeral CI containers without a persistent home volume, checkpoints do not survive between runs — `--resume` will have nothing to restore from.

For persistent checkpointing in CI, mount a volume at `$HOME/.clausura/`.

## Best Practices

1. **Use a committed review range** — Fetch full history and the target ref, then pass `--base <ref-or-sha>`. The normal CLI without `--base` retains local working-tree diff behavior; CI metadata detection alone does not select a PR range.

2. **Use secrets for API keys** — Never commit API keys. Use your CI's secrets manager (`${{ secrets.LLM_API_KEY }}`, GitLab CI/CD variables, Jenkins credentials).

3. **Upload SARIF on failure** — Use `if: always()` so SARIF is available for debugging even when the pipeline fails.

4. **Set a job timeout** — Allow time for installation and uploads. Sharded runs currently apply budgets per shard/attempt, so the overall CI timeout must bound the aggregate run.

5. **Choose the intended range** — On push/manual runs, supply an explicit base appropriate to the changes being reviewed.

## Next

→ [Troubleshooting common issues](troubleshooting.md)
