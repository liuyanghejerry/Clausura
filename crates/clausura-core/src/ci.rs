use std::env;
use std::fmt;

use crate::types::CiContext;

/// Detected CI provider.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CiProvider {
    GitHubActions,
    GitLabCi,
    Jenkins,
    Generic,
    Local,
}

impl fmt::Display for CiProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CiProvider::GitHubActions => write!(f, "GitHub Actions"),
            CiProvider::GitLabCi => write!(f, "GitLab CI"),
            CiProvider::Jenkins => write!(f, "Jenkins"),
            CiProvider::Generic => write!(f, "Generic CI"),
            CiProvider::Local => write!(f, "Local"),
        }
    }
}

/// Detect the current CI provider by inspecting well-known environment variables.
///
/// Detection order:
/// 1. `GITHUB_ACTIONS` -> GitHub Actions
/// 2. `GITLAB_CI` -> GitLab CI
/// 3. `JENKINS_URL` -> Jenkins
/// 4. `CI=true` or `CI=1` -> Generic CI
/// 5. Otherwise -> Local
pub fn detect() -> CiProvider {
    if env::var("GITHUB_ACTIONS").is_ok() {
        CiProvider::GitHubActions
    } else if env::var("GITLAB_CI").is_ok() {
        CiProvider::GitLabCi
    } else if env::var("JENKINS_URL").is_ok() {
        CiProvider::Jenkins
    } else if env::var("CI").is_ok_and(|v| v == "true" || v == "1") {
        CiProvider::Generic
    } else {
        CiProvider::Local
    }
}

/// Returns `true` when running inside any CI environment.
pub fn is_ci() -> bool {
    detect() != CiProvider::Local
}

/// Gather CI context (repo, PR number, commit SHA, branch) from the
/// environment variables that are specific to the detected provider.
pub fn context() -> CiContext {
    match detect() {
        CiProvider::GitHubActions => CiContext {
            repo: env::var("GITHUB_REPOSITORY").ok(),
            pr_number: env::var("GITHUB_PR_NUMBER")
                .ok()
                .or_else(|| env::var("PR_NUMBER").ok())
                .or_else(|| {
                    env::var("GITHUB_REF").ok().and_then(|ref_val| {
                        ref_val
                            .strip_prefix("refs/pull/")
                            .and_then(|s| s.split('/').next())
                            .map(|s| s.to_string())
                    })
                }),
            commit_sha: env::var("GITHUB_SHA").ok(),
            branch: env::var("GITHUB_HEAD_REF")
                .or_else(|_| env::var("GITHUB_REF_NAME"))
                .ok(),
        },
        CiProvider::GitLabCi => CiContext {
            repo: env::var("CI_PROJECT_PATH").ok(),
            pr_number: env::var("CI_MERGE_REQUEST_IID").ok(),
            commit_sha: env::var("CI_COMMIT_SHA").ok(),
            branch: env::var("CI_COMMIT_BRANCH")
                .or_else(|_| env::var("CI_MERGE_REQUEST_SOURCE_BRANCH_NAME"))
                .ok(),
        },
        CiProvider::Jenkins => CiContext {
            repo: env::var("JOB_NAME")
                .ok()
                .or_else(|| env::var("GIT_URL").ok())
                .or_else(|| env::var("JOB_DISPLAY_URL").ok()),
            pr_number: env::var("CHANGE_ID")
                .or_else(|_| env::var("ghprbPullId"))
                .ok(),
            commit_sha: env::var("GIT_COMMIT").ok(),
            branch: env::var("BRANCH_NAME")
                .or_else(|_| env::var("GIT_BRANCH"))
                .or_else(|_| env::var("CHANGE_BRANCH"))
                .ok(),
        },
        CiProvider::Generic | CiProvider::Local => CiContext {
            repo: env::var("CI_REPO").ok(),
            pr_number: env::var("CI_PR_NUMBER").ok(),
            commit_sha: env::var("CI_COMMIT_SHA")
                .or_else(|_| env::var("COMMIT_SHA"))
                .ok(),
            branch: env::var("CI_BRANCH").or_else(|_| env::var("BRANCH")).ok(),
        },
    }
}

/// Render a template string with CI context values.
///
/// Supported placeholders:
/// - `{{repo}}` -- repository identifier
/// - `{{pr_number}}` -- pull / merge request number
/// - `{{commit_sha}}` -- commit SHA
/// - `{{branch}}` -- current branch
///
/// Unknown or unset placeholders are left as-is in the output.
pub fn render_template(template: &str) -> String {
    let ctx = context();
    let mut result = template.to_string();

    if let Some(ref repo) = ctx.repo {
        result = result.replace("{{repo}}", repo);
    }
    if let Some(ref pr) = ctx.pr_number {
        result = result.replace("{{pr_number}}", pr);
    }
    if let Some(ref sha) = ctx.commit_sha {
        result = result.replace("{{commit_sha}}", sha);
    }
    if let Some(ref branch) = ctx.branch {
        result = result.replace("{{branch}}", branch);
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_ci_env(vars: &[(&str, &str)], f: impl FnOnce()) {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let ci_keys = [
            "GITHUB_ACTIONS",
            "GITHUB_REPOSITORY",
            "GITHUB_SHA",
            "GITHUB_REF_NAME",
            "GITHUB_HEAD_REF",
            "GITHUB_REF",
            "GITHUB_PR_NUMBER",
            "PR_NUMBER",
            "GITLAB_CI",
            "CI_PROJECT_PATH",
            "CI_COMMIT_SHA",
            "CI_MERGE_REQUEST_IID",
            "CI_COMMIT_BRANCH",
            "CI_MERGE_REQUEST_SOURCE_BRANCH_NAME",
            "JENKINS_URL",
            "JOB_NAME",
            "GIT_URL",
            "JOB_DISPLAY_URL",
            "GIT_COMMIT",
            "BRANCH_NAME",
            "GIT_BRANCH",
            "CHANGE_BRANCH",
            "CHANGE_ID",
            "ghprbPullId",
            "CI",
            "CI_REPO",
            "CI_PR_NUMBER",
            "CI_BRANCH",
            "COMMIT_SHA",
            "BRANCH",
        ];
        let saved: HashMap<&str, Option<String>> = ci_keys
            .iter()
            .map(|k| (*k, std::env::var(k).ok()))
            .collect();

        // First: clear ALL CI-related vars
        for k in &ci_keys {
            unsafe { std::env::remove_var(k) };
        }

        // Then: set only the test-specific vars
        for (k, v) in vars {
            unsafe {
                std::env::set_var(k, *v);
            }
        }

        f();

        // Restore original state
        for k in &ci_keys {
            match saved.get(k).unwrap() {
                Some(v) => unsafe { std::env::set_var(k, v) },
                None => unsafe { std::env::remove_var(k) },
            }
        }
    }

    #[test]
    fn test_detect_github_actions() {
        with_ci_env(&[("GITHUB_ACTIONS", "true")], || {
            assert_eq!(detect(), CiProvider::GitHubActions);
        });
    }

    #[test]
    fn test_detect_gitlab_ci() {
        with_ci_env(&[("GITLAB_CI", "true")], || {
            assert_eq!(detect(), CiProvider::GitLabCi);
        });
    }

    #[test]
    fn test_detect_jenkins() {
        with_ci_env(&[("JENKINS_URL", "http://jenkins.example.com")], || {
            assert_eq!(detect(), CiProvider::Jenkins);
        });
    }

    #[test]
    fn test_detect_generic_ci() {
        with_ci_env(&[("CI", "true")], || {
            assert_eq!(detect(), CiProvider::Generic);
        });
    }

    #[test]
    fn test_detect_generic_ci_one() {
        with_ci_env(&[("CI", "1")], || {
            assert_eq!(detect(), CiProvider::Generic);
        });
    }

    #[test]
    fn test_detect_local() {
        with_ci_env(&[], || {
            assert_eq!(detect(), CiProvider::Local);
        });
    }

    #[test]
    fn test_is_ci_true() {
        with_ci_env(&[("CI", "true")], || {
            assert!(is_ci());
        });
    }

    #[test]
    fn test_is_ci_false() {
        with_ci_env(&[], || {
            assert!(!is_ci());
        });
    }

    #[test]
    fn test_ci_provider_display() {
        assert_eq!(CiProvider::GitHubActions.to_string(), "GitHub Actions");
        assert_eq!(CiProvider::GitLabCi.to_string(), "GitLab CI");
        assert_eq!(CiProvider::Jenkins.to_string(), "Jenkins");
        assert_eq!(CiProvider::Generic.to_string(), "Generic CI");
        assert_eq!(CiProvider::Local.to_string(), "Local");
    }

    #[test]
    fn test_detection_order_github_over_generic() {
        with_ci_env(&[("GITHUB_ACTIONS", "true"), ("CI", "true")], || {
            assert_eq!(detect(), CiProvider::GitHubActions);
        });
    }

    #[test]
    fn test_context_github_actions() {
        with_ci_env(
            &[
                ("GITHUB_ACTIONS", "true"),
                ("GITHUB_REPOSITORY", "owner/repo"),
                ("GITHUB_REF", "refs/pull/42/merge"),
                ("GITHUB_SHA", "abc123def456"),
                ("GITHUB_HEAD_REF", "feature-branch"),
            ],
            || {
                let ctx = context();
                assert_eq!(ctx.repo.as_deref(), Some("owner/repo"));
                assert_eq!(ctx.pr_number.as_deref(), Some("42"));
                assert_eq!(ctx.commit_sha.as_deref(), Some("abc123def456"));
                assert_eq!(ctx.branch.as_deref(), Some("feature-branch"));
            },
        );
    }

    #[test]
    fn test_context_gitlab_ci() {
        with_ci_env(
            &[
                ("GITLAB_CI", "true"),
                ("CI_PROJECT_PATH", "group/project"),
                ("CI_MERGE_REQUEST_IID", "7"),
                ("CI_COMMIT_SHA", "deadbeef"),
                ("CI_COMMIT_BRANCH", "main"),
            ],
            || {
                let ctx = context();
                assert_eq!(ctx.repo.as_deref(), Some("group/project"));
                assert_eq!(ctx.pr_number.as_deref(), Some("7"));
                assert_eq!(ctx.commit_sha.as_deref(), Some("deadbeef"));
                assert_eq!(ctx.branch.as_deref(), Some("main"));
            },
        );
    }

    #[test]
    fn test_context_jenkins() {
        with_ci_env(
            &[
                ("JENKINS_URL", "http://jenkins:8080"),
                ("JOB_NAME", "my-job"),
                ("CHANGE_ID", "99"),
                ("GIT_COMMIT", "feedcafe"),
                ("BRANCH_NAME", "develop"),
            ],
            || {
                let ctx = context();
                assert_eq!(ctx.pr_number.as_deref(), Some("99"));
                assert_eq!(ctx.commit_sha.as_deref(), Some("feedcafe"));
                assert_eq!(ctx.branch.as_deref(), Some("develop"));
            },
        );
    }

    #[test]
    fn test_context_generic() {
        with_ci_env(
            &[
                ("CI", "true"),
                ("CI_REPO", "custom/repo"),
                ("CI_PR_NUMBER", "123"),
                ("CI_COMMIT_SHA", "cafebabe"),
                ("CI_BRANCH", "staging"),
            ],
            || {
                let ctx = context();
                assert_eq!(ctx.repo.as_deref(), Some("custom/repo"));
                assert_eq!(ctx.pr_number.as_deref(), Some("123"));
                assert_eq!(ctx.commit_sha.as_deref(), Some("cafebabe"));
                assert_eq!(ctx.branch.as_deref(), Some("staging"));
            },
        );
    }

    #[test]
    fn test_context_local_empty() {
        with_ci_env(&[], || {
            let ctx = context();
            assert!(ctx.repo.is_none());
            assert!(ctx.pr_number.is_none());
            assert!(ctx.commit_sha.is_none());
            assert!(ctx.branch.is_none());
        });
    }

    #[test]
    fn test_render_template_all_fields() {
        with_ci_env(
            &[
                ("CI", "true"),
                ("CI_REPO", "owner/repo"),
                ("CI_PR_NUMBER", "42"),
                ("CI_COMMIT_SHA", "abc123"),
                ("CI_BRANCH", "main"),
            ],
            || {
                let rendered = render_template("{{repo}}/{{pr_number}}/{{commit_sha}}/{{branch}}");
                assert_eq!(rendered, "owner/repo/42/abc123/main");
            },
        );
    }

    #[test]
    fn test_render_template_partial() {
        with_ci_env(&[("CI", "true"), ("CI_REPO", "owner/repo")], || {
            let rendered = render_template("Repo: {{repo}}, PR: {{pr_number}}");
            assert_eq!(rendered, "Repo: owner/repo, PR: {{pr_number}}");
        });
    }

    #[test]
    fn test_render_template_no_ci() {
        with_ci_env(&[], || {
            let rendered = render_template("{{repo}}-{{branch}}");
            assert_eq!(rendered, "{{repo}}-{{branch}}");
        });
    }
}
