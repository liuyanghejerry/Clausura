// ---------------------------------------------------------------------------
// Findings verification via a System One decision model (TypeSafe Jev)
// ---------------------------------------------------------------------------
//
// Optional pre-gating filter: each finding is scored by the decision model
// with a single yes/no question, and findings whose calibrated probability
// falls below the configured threshold are excluded from gating. The review
// LLM still does all open-ended analysis — the decision model only audits
// its output. Everything here is fail-open: a missing API key, a request
// error, or an unparseable response keeps the finding and warns, so a
// decision-API outage can never block CI.
// ---------------------------------------------------------------------------

use crate::provider::{create_provider_with_options, Provider, ProviderOptions};
use crate::types::{
    FilteredFinding, Finding, Message, Role, VerificationSummary, VerifyConfig, VerifyProvider,
};
use async_trait::async_trait;
use std::path::Path;
use std::sync::Arc;

/// Bounded concurrency for verification calls. Findings per run are small
/// (capped by `VerifyConfig::max_findings`), so a fixed window keeps the
/// code simple without unbounded task spawning.
const CONCURRENCY: usize = 8;

const QUESTION_ID: &str = "supported";

const VERIFY_QUESTION: &str = "Is this code-review finding genuine and supported by its evidence?";

const VERIFY_INSTRUCTIONS: &str = "You are auditing findings from an automated code review. \
Answer yes only if the finding is internally consistent and credible: the message describes a \
concrete code issue, and the evidence and location (when present) support that specific claim. \
Answer no when the claim is vague, generic, boilerplate, self-contradictory, or unsupported by \
its own evidence.";

#[derive(Debug, thiserror::Error)]
pub enum JevError {
    #[error("decision API request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("decision API returned HTTP {status}: {body}")]
    Api { status: u16, body: String },
    #[error("decision API response missing noul probability: {0}")]
    Parse(String),
    #[error("judge provider error: {0}")]
    Provider(String),
}

/// Pluggable pre-gating finding verifier returning `P(finding is genuine) ∈ [0, 1]`.
#[async_trait]
pub trait FindingVerifier: Send + Sync {
    async fn score(&self, finding: &Finding, source_context: Option<&str>)
        -> Result<f64, JevError>;

    fn model_name(&self) -> &str;
}

/// Minimal client for the System One decisions API (`POST /v1/systemone`).
/// Deliberately separate from the `Provider` trait: the decision API is not
/// chat-shaped (`state` + typed `questions` in, probabilities out).
#[derive(Clone)]
pub struct JevClient {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
}

#[async_trait]
impl FindingVerifier for JevClient {
    async fn score(
        &self,
        finding: &Finding,
        _source_context: Option<&str>,
    ) -> Result<f64, JevError> {
        self.verify_one(finding).await
    }

    fn model_name(&self) -> &str {
        &self.model
    }
}

/// LLM-as-a-Judge verifier that inspects both the finding and surrounding
/// source code lines (when available) to filter out false positives.
pub struct LlmJudgeVerifier {
    provider: Arc<dyn Provider>,
}

impl LlmJudgeVerifier {
    pub fn new(provider: Arc<dyn Provider>) -> Self {
        Self { provider }
    }

    fn parse_judge_probability(content: &str) -> Result<f64, JevError> {
        let trimmed = content.trim();
        let val: serde_json::Value = serde_json::from_str(trimmed)
            .or_else(|_| {
                let start = trimmed.find('{').ok_or(())?;
                let end = trimmed.rfind('}').ok_or(())?;
                serde_json::from_str(&trimmed[start..=end]).map_err(|_| ())
            })
            .map_err(|_| JevError::Parse(trimmed.chars().take(300).collect()))?;

        if let Some(p) = val
            .get("probability")
            .and_then(|v| v.as_f64())
            .filter(|p| (0.0..=1.0).contains(p))
        {
            return Ok(p);
        }
        if let Some(genuine) = val.get("genuine").and_then(|v| v.as_bool()) {
            let conf = val
                .get("confidence")
                .and_then(|v| v.as_f64())
                .filter(|c| (0.0..=1.0).contains(c))
                .unwrap_or(0.9);
            return Ok(if genuine { conf } else { 1.0 - conf });
        }
        Err(JevError::Parse(truncate(&val)))
    }
}

#[async_trait]
impl FindingVerifier for LlmJudgeVerifier {
    async fn score(
        &self,
        finding: &Finding,
        source_context: Option<&str>,
    ) -> Result<f64, JevError> {
        let finding_json = serde_json::to_string_pretty(finding).unwrap_or_default();
        let mut user_msg = format!("Finding to verify:\n```json\n{finding_json}\n```\n");
        if let Some(src) = source_context {
            user_msg.push_str("\nSurrounding source code (with line numbers):\n```\n");
            user_msg.push_str(src);
            user_msg.push_str("\n```\n");
        }
        let messages = vec![
            Message::new(
                Role::System,
                "You are a strict code-review finding auditor. Verify whether the finding is \
                 genuinely present and factually supported by the code context (when provided) \
                 and evidence. Reply with ONLY a JSON object: \
                 {\"probability\": <0.0-1.0>, \"genuine\": <true|false>, \"reason\": \"<brief>\"} \
                 where `probability` is P(the finding is a real, non-false-positive issue).",
            ),
            Message::new(Role::User, user_msg),
        ];
        let resp = self
            .provider
            .chat(&messages)
            .await
            .map_err(|e| JevError::Provider(e.to_string()))?;
        Self::parse_judge_probability(&resp.message.content)
    }

    fn model_name(&self) -> &str {
        self.provider.model()
    }
}

/// Read up to `radius` lines before and after `finding.location` inside
/// `workspace` (sandboxed — rejects paths escaping the workspace root).
pub fn read_source_context(workspace: &Path, finding: &Finding, radius: usize) -> Option<String> {
    let loc = finding.location.as_ref()?;
    if loc.line_start == 0 {
        return None;
    }
    let canon_root = workspace.canonicalize().ok()?;
    let rel = loc.file.trim().trim_start_matches("./").replace('\\', "/");
    let full = canon_root.join(&rel).canonicalize().ok()?;
    if !full.starts_with(&canon_root) {
        return None;
    }
    let content = std::fs::read_to_string(full).ok()?;
    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() {
        return None;
    }
    let start_1 = (loc.line_start as usize).saturating_sub(radius).max(1);
    let end_1 = (loc.line_end.max(loc.line_start) as usize + radius).min(lines.len());
    if start_1 > lines.len() {
        return None;
    }
    let mut out = String::new();
    for (idx, line) in lines[(start_1 - 1)..end_1].iter().enumerate() {
        let line_no = start_1 + idx;
        out.push_str(&format!("{line_no:>5} | {line}\n"));
    }
    Some(out)
}

impl JevClient {
    pub fn new(config: &VerifyConfig, api_key: String) -> Self {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(config.timeout_secs))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            http,
            base_url: config.base_url.trim_end_matches('/').to_string(),
            api_key,
            model: config.model.clone(),
        }
    }

    fn request_body(&self, finding: &Finding) -> serde_json::Value {
        serde_json::json!({
            "model": self.model,
            "state": { "finding": finding },
            "questions": {
                QUESTION_ID: {
                    "type": "noul",
                    "question": VERIFY_QUESTION,
                    "instructions": VERIFY_INSTRUCTIONS,
                }
            }
        })
    }

    /// Extract P(finding is genuine). Tolerates both the documented wrapped
    /// shape (`{"answers": {"supported": {"noul": ...}}}`) and a flat shape
    /// (`{"supported": {"noul": ...}}`) some gateways return.
    fn parse_probability(body: &serde_json::Value) -> Result<f64, JevError> {
        let answer = body
            .get("answers")
            .and_then(|a| a.get(QUESTION_ID))
            .or_else(|| body.get(QUESTION_ID))
            .ok_or_else(|| JevError::Parse(truncate(body)))?;
        answer
            .get("noul")
            .and_then(|n| n.as_f64())
            .filter(|p| (0.0..=1.0).contains(p))
            .ok_or_else(|| JevError::Parse(truncate(body)))
    }

    pub async fn verify_one(&self, finding: &Finding) -> Result<f64, JevError> {
        let resp = self
            .http
            .post(format!("{}/v1/systemone", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&self.request_body(finding))
            .send()
            .await?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(JevError::Api {
                status: status.as_u16(),
                body: text.chars().take(500).collect(),
            });
        }
        let body: serde_json::Value = serde_json::from_str(&text)
            .map_err(|_| JevError::Parse(text.chars().take(500).collect()))?;
        Self::parse_probability(&body)
    }
}

fn truncate(v: &serde_json::Value) -> String {
    v.to_string().chars().take(500).collect()
}

/// Score findings with the configured verifier (`jev` or `llm`) and split out
/// those below `threshold`. Returns the findings to gate on, plus a summary
/// (`None` when verification did not run).
pub async fn verify_findings(
    config: &VerifyConfig,
    findings: Vec<Finding>,
) -> (Vec<Finding>, Option<VerificationSummary>) {
    verify_findings_with_context(config, findings, None, None).await
}

/// Score findings with optional workspace source context and fallback LLM
/// provider (reused when `verify.provider == llm` and no separate judge vendor
/// is configured).
pub async fn verify_findings_with_context(
    config: &VerifyConfig,
    findings: Vec<Finding>,
    workspace: Option<&Path>,
    fallback_provider: Option<Arc<dyn Provider>>,
) -> (Vec<Finding>, Option<VerificationSummary>) {
    if !config.enabled || findings.is_empty() {
        return (findings, None);
    }

    let verifier: Arc<dyn FindingVerifier> = match config.provider {
        VerifyProvider::Jev => {
            let api_key = match std::env::var(&config.api_key_env) {
                Ok(k) if !k.trim().is_empty() => k,
                _ => {
                    eprintln!(
                        "Warning: verify.enabled is true but ${} is not set; skipping findings verification",
                        config.api_key_env
                    );
                    return (findings, None);
                }
            };
            Arc::new(JevClient::new(config, api_key))
        }
        VerifyProvider::Llm => {
            let judge_provider: Arc<dyn Provider> = if let Some(ref vendor) = config.vendor {
                let api_key = vendor
                    .api_key_env
                    .as_deref()
                    .and_then(|n| std::env::var(n).ok())
                    .filter(|v| !v.trim().is_empty())
                    .or_else(|| {
                        std::env::var(&config.api_key_env)
                            .ok()
                            .filter(|v| !v.trim().is_empty())
                    })
                    .or_else(|| {
                        std::env::var("CLAUSURA_API_KEY")
                            .ok()
                            .filter(|v| !v.trim().is_empty())
                    })
                    .unwrap_or_default();
                let opts = ProviderOptions {
                    max_output_tokens: Some(512),
                    temperature: Some(0.0),
                    prompt_caching: false,
                };
                match create_provider_with_options(
                    vendor,
                    &config.model,
                    &api_key,
                    config.timeout_secs,
                    &opts,
                ) {
                    Ok(p) => Arc::from(p),
                    Err(e) => {
                        eprintln!(
                            "Warning: failed to initialize LLM verifier ({e}); skipping findings verification"
                        );
                        return (findings, None);
                    }
                }
            } else if let Some(p) = fallback_provider {
                p
            } else {
                eprintln!(
                    "Warning: verify.provider is 'llm' without verify.vendor or fallback provider; skipping findings verification"
                );
                return (findings, None);
            };
            Arc::new(LlmJudgeVerifier::new(judge_provider))
        }
    };

    verify_with_verifier(
        verifier,
        findings,
        config.threshold,
        config.max_findings,
        workspace,
        config.context_lines,
    )
    .await
}

/// Core bounded-concurrency verification fan-out over any [`FindingVerifier`].
pub async fn verify_with_verifier(
    verifier: Arc<dyn FindingVerifier>,
    findings: Vec<Finding>,
    threshold: f64,
    max_findings: usize,
    workspace: Option<&Path>,
    context_lines: usize,
) -> (Vec<Finding>, Option<VerificationSummary>) {
    if findings.is_empty() {
        return (findings, None);
    }
    let threshold = threshold.clamp(0.0, 1.0);
    let model_name = verifier.model_name().to_string();

    let mut iter = findings.into_iter();
    let to_verify: Vec<Finding> = iter.by_ref().take(max_findings).collect();
    let skipped: Vec<Finding> = iter.collect();

    let contexts: Vec<Option<String>> = to_verify
        .iter()
        .map(|f| workspace.and_then(|ws| read_source_context(ws, f, context_lines)))
        .collect();

    // Bounded-concurrency fan-out: window of CONCURRENCY in-flight calls.
    let mut results: Vec<Option<Result<f64, JevError>>> =
        (0..to_verify.len()).map(|_| None).collect();
    let mut set: tokio::task::JoinSet<(usize, Result<f64, JevError>)> = tokio::task::JoinSet::new();
    let mut next = 0;
    while next < to_verify.len() || !set.is_empty() {
        while next < to_verify.len() && set.len() < CONCURRENCY {
            let v = verifier.clone();
            let finding = to_verify[next].clone();
            let ctx = contexts[next].clone();
            let idx = next;
            set.spawn(async move { (idx, v.score(&finding, ctx.as_deref()).await) });
            next += 1;
        }
        if let Some(Ok((idx, res))) = set.join_next().await {
            results[idx] = Some(res);
        }
    }

    let mut kept: Vec<Finding> = Vec::new();
    let mut filtered: Vec<FilteredFinding> = Vec::new();
    let mut verified = 0usize;
    let mut failed = 0usize;
    for (finding, res) in to_verify.into_iter().zip(results) {
        match res {
            Some(Ok(p)) if p >= threshold => {
                verified += 1;
                kept.push(finding);
            }
            Some(Ok(p)) => filtered.push(FilteredFinding {
                finding_id: finding.id,
                rule_id: finding.rule_id.clone(),
                probability: p,
                message: finding.message.chars().take(200).collect(),
            }),
            // Fail-open: request/parse failures (and join failures leaving a
            // `None` slot) keep the finding.
            Some(Err(e)) => {
                failed += 1;
                eprintln!(
                    "Warning: verification call failed for finding {} ({}); keeping finding",
                    finding.id, e
                );
                kept.push(finding);
            }
            None => {
                failed += 1;
                kept.push(finding);
            }
        }
    }

    if !filtered.is_empty() {
        eprintln!(
            "Verification filtered {} finding(s) below threshold {:.2}:",
            filtered.len(),
            threshold
        );
        for f in &filtered {
            eprintln!("  - [{}] {} (p={:.2})", f.rule_id, f.message, f.probability);
        }
    }

    let skipped_n = skipped.len();
    kept.extend(skipped);
    let summary = VerificationSummary {
        model: model_name,
        threshold,
        verified,
        filtered: filtered.len(),
        failed,
        skipped: skipped_n,
        filtered_findings: filtered,
    };
    (kept, Some(summary))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Severity, VerifyConfig};
    use uuid::Uuid;
    use wiremock::{matchers, Mock, MockServer, ResponseTemplate};

    fn make_finding(rule_id: &str) -> Finding {
        Finding {
            id: Uuid::new_v4(),
            rule_id: rule_id.to_string(),
            severity: Severity::Warning,
            message: format!("issue in {rule_id}"),
            location: None,
            evidence: "evidence".into(),
        }
    }

    fn noul_body(p: f64) -> serde_json::Value {
        serde_json::json!({
            "model": "jev-1.13.0",
            "answers": { QUESTION_ID: { "type": "noul", "noul": p } },
            "usage": { "input_tokens": 100, "output_tokens": 5 },
        })
    }

    #[test]
    fn test_parse_probability_wrapped_and_flat() {
        let wrapped = noul_body(0.95);
        assert_eq!(JevClient::parse_probability(&wrapped).unwrap(), 0.95);
        let flat = serde_json::json!({ QUESTION_ID: { "type": "noul", "noul": 0.1 } });
        assert_eq!(JevClient::parse_probability(&flat).unwrap(), 0.1);
    }

    #[test]
    fn test_parse_probability_rejects_garbage() {
        assert!(JevClient::parse_probability(&serde_json::json!({})).is_err());
        assert!(JevClient::parse_probability(
            &serde_json::json!({"answers": {"supported": {"noul": 1.7}}})
        )
        .is_err());
    }

    #[tokio::test]
    async fn test_disabled_passthrough() {
        let config = VerifyConfig::default();
        let findings = vec![make_finding("a")];
        let (kept, summary) = verify_findings(&config, findings).await;
        assert_eq!(kept.len(), 1);
        assert!(summary.is_none());
    }

    #[tokio::test]
    async fn test_missing_api_key_passthrough() {
        let config = VerifyConfig {
            enabled: true,
            api_key_env: "JEV_TEST_KEY_DEFINITELY_UNSET".into(),
            ..Default::default()
        };
        let findings = vec![make_finding("a")];
        let (kept, summary) = verify_findings(&config, findings).await;
        assert_eq!(kept.len(), 1);
        assert!(summary.is_none());
    }

    #[tokio::test]
    async fn test_filters_below_threshold() {
        let mock_server = MockServer::start().await;
        Mock::given(matchers::method("POST"))
            .and(matchers::path("/v1/systemone"))
            .and(matchers::header("authorization", "Bearer test-key"))
            .respond_with(|req: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
                assert_eq!(body["model"], "jev-latest");
                assert_eq!(body["questions"][QUESTION_ID]["type"], "noul");
                let rule = body["state"]["finding"]["rule_id"].as_str().unwrap();
                let p = if rule == "genuine" { 0.9 } else { 0.2 };
                ResponseTemplate::new(200).set_body_json(noul_body(p))
            })
            .mount(&mock_server)
            .await;

        std::env::set_var("JEV_TEST_KEY_FILTER", "test-key");
        let config = VerifyConfig {
            enabled: true,
            base_url: mock_server.uri(),
            api_key_env: "JEV_TEST_KEY_FILTER".into(),
            ..Default::default()
        };
        let findings = vec![make_finding("genuine"), make_finding("bogus")];
        let (kept, summary) = verify_findings(&config, findings).await;
        std::env::remove_var("JEV_TEST_KEY_FILTER");

        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].rule_id, "genuine");
        let summary = summary.unwrap();
        assert_eq!(summary.verified, 1);
        assert_eq!(summary.filtered, 1);
        assert_eq!(summary.failed, 0);
        assert_eq!(summary.filtered_findings.len(), 1);
        assert_eq!(summary.filtered_findings[0].rule_id, "bogus");
        assert!((summary.filtered_findings[0].probability - 0.2).abs() < 1e-9);
    }

    #[tokio::test]
    async fn test_api_error_is_fail_open() {
        let mock_server = MockServer::start().await;
        Mock::given(matchers::method("POST"))
            .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
            .mount(&mock_server)
            .await;

        std::env::set_var("JEV_TEST_KEY_ERR", "test-key");
        let config = VerifyConfig {
            enabled: true,
            base_url: mock_server.uri(),
            api_key_env: "JEV_TEST_KEY_ERR".into(),
            ..Default::default()
        };
        let findings = vec![make_finding("a"), make_finding("b")];
        let (kept, summary) = verify_findings(&config, findings).await;
        std::env::remove_var("JEV_TEST_KEY_ERR");

        assert_eq!(kept.len(), 2);
        let summary = summary.unwrap();
        assert_eq!(summary.failed, 2);
        assert_eq!(summary.filtered, 0);
    }

    #[tokio::test]
    async fn test_max_findings_cap_skips_rest() {
        let mock_server = MockServer::start().await;
        Mock::given(matchers::method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(noul_body(0.9)))
            .expect(2)
            .mount(&mock_server)
            .await;

        std::env::set_var("JEV_TEST_KEY_CAP", "test-key");
        let config = VerifyConfig {
            enabled: true,
            base_url: mock_server.uri(),
            api_key_env: "JEV_TEST_KEY_CAP".into(),
            max_findings: 2,
            ..Default::default()
        };
        let findings = vec![make_finding("a"), make_finding("b"), make_finding("c")];
        let (kept, summary) = verify_findings(&config, findings).await;
        std::env::remove_var("JEV_TEST_KEY_CAP");

        assert_eq!(kept.len(), 3);
        let summary = summary.unwrap();
        assert_eq!(summary.verified, 2);
        assert_eq!(summary.skipped, 1);
    }

    #[tokio::test]
    async fn test_llm_judge_verifier_uses_source_context_and_filters_false_positive() {
        struct ContextAwareJudge;

        #[async_trait]
        impl Provider for ContextAwareJudge {
            async fn chat(
                &self,
                messages: &[Message],
            ) -> Result<crate::types::ChatResponse, crate::types::ProviderError> {
                let prompt = &messages[1].content;
                let reply = if prompt.contains("3 |     let q = format!") {
                    r#"{"probability": 0.92, "genuine": true, "reason": "confirmed format! SQL"}"#
                } else {
                    r#"{"probability": 0.12, "genuine": false, "reason": "false positive"}"#
                };
                Ok(crate::types::ChatResponse {
                    message: Message::new(Role::Assistant, reply),
                    usage: crate::types::Usage::default(),
                    finish_reason: crate::types::FinishReason::Stop,
                    tool_calls: None,
                })
            }

            async fn chat_with_tools(
                &self,
                messages: &[Message],
                _tools: &[crate::types::ToolDef],
            ) -> Result<crate::types::ChatResponse, crate::types::ProviderError> {
                self.chat(messages).await
            }

            fn count_tokens(&self, text: &str) -> u64 {
                text.len() as u64 / 4
            }

            fn model(&self) -> &str {
                "judge-mini"
            }

            fn vendor(&self) -> &str {
                "mock"
            }
        }

        let tmp = tempfile::TempDir::new().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("db.rs"),
            "fn a() {}\nfn b() {\n    let q = format!(\"SELECT * FROM t WHERE id={}\", id);\n}\n",
        )
        .unwrap();

        let mut real_finding = make_finding("sql-injection");
        real_finding.location = Some(crate::types::Location {
            file: "src/db.rs".into(),
            line_start: 3,
            line_end: 3,
            column_start: 1,
            column_end: 1,
        });
        let fp_finding = make_finding("bogus");

        let config = VerifyConfig {
            enabled: true,
            provider: VerifyProvider::Llm,
            threshold: 0.5,
            ..Default::default()
        };
        let (kept, summary) = verify_findings_with_context(
            &config,
            vec![real_finding, fp_finding],
            Some(tmp.path()),
            Some(Arc::new(ContextAwareJudge)),
        )
        .await;

        let summary = summary.unwrap();
        assert_eq!(summary.model, "judge-mini");
        assert_eq!(summary.verified, 1);
        assert_eq!(summary.filtered, 1);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].rule_id, "sql-injection");
    }
}
