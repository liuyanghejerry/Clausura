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

use crate::types::{FilteredFinding, Finding, VerificationSummary, VerifyConfig};

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

/// Score findings with the decision model and split out the ones below
/// `threshold`. Returns the findings to gate on, plus a summary (`None` when
/// verification did not run: disabled, no findings, or no API key).
pub async fn verify_findings(
    config: &VerifyConfig,
    findings: Vec<Finding>,
) -> (Vec<Finding>, Option<VerificationSummary>) {
    if !config.enabled || findings.is_empty() {
        return (findings, None);
    }
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
    let client = JevClient::new(config, api_key);
    let threshold = config.threshold.clamp(0.0, 1.0);

    let mut iter = findings.into_iter();
    let to_verify: Vec<Finding> = iter.by_ref().take(config.max_findings).collect();
    let skipped: Vec<Finding> = iter.collect();

    // Bounded-concurrency fan-out: window of CONCURRENCY in-flight calls.
    let mut results: Vec<Option<Result<f64, JevError>>> =
        (0..to_verify.len()).map(|_| None).collect();
    let mut set: tokio::task::JoinSet<(usize, Result<f64, JevError>)> = tokio::task::JoinSet::new();
    let mut next = 0;
    while next < to_verify.len() || !set.is_empty() {
        while next < to_verify.len() && set.len() < CONCURRENCY {
            let client = client.clone();
            let finding = to_verify[next].clone();
            let idx = next;
            set.spawn(async move { (idx, client.verify_one(&finding).await) });
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
        model: config.model.clone(),
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
}
