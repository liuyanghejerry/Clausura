use crate::types::{Finding, Severity};
use std::fs;
use std::path::Path;

/// SARIF v2.1.0 output formatter.
pub struct SarifFormatter;

impl SarifFormatter {
    /// Convert findings to SARIF JSON string.
    pub fn to_string(findings: &[Finding]) -> Result<String, serde_json::Error> {
        let sarif = Self::build_sarif(findings);
        serde_json::to_string_pretty(&sarif)
    }

    /// Write SARIF output to a file.
    pub fn write_to_file(
        findings: &[Finding],
        path: &Path,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let content = Self::to_string(findings)?;
        fs::write(path, content)?;
        Ok(())
    }

    fn build_sarif(findings: &[Finding]) -> serde_json::Value {
        let results: Vec<serde_json::Value> =
            findings.iter().map(Self::finding_to_result).collect();

        serde_json::json!({
            "$schema": "https://json.schemastore.org/sarif-2.1.0.json",
            "version": "2.1.0",
            "runs": [{
                "tool": {
                    "driver": {
                        "name": "Clausura",
                        "informationUri": "https://github.com/clausura/clausura"
                    }
                },
                "results": results
            }]
        })
    }

    fn severity_to_level(severity: &Severity) -> &'static str {
        match severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Info => "note",
            Severity::Hint => "none",
        }
    }

    fn finding_to_result(finding: &Finding) -> serde_json::Value {
        let mut result = serde_json::json!({
            "ruleId": finding.rule_id,
            "level": Self::severity_to_level(&finding.severity),
            "message": {
                "text": finding.message
            }
        });

        if let Some(loc) = &finding.location {
            result["locations"] = serde_json::json!([{
                "physicalLocation": {
                    "artifactLocation": {
                        "uri": loc.file
                    },
                    "region": {
                        "startLine": loc.line_start,
                        "endLine": loc.line_end
                    }
                }
            }]);
        }

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Location, Severity};
    use uuid::Uuid;

    fn make_finding(severity: Severity, has_location: bool) -> Finding {
        Finding {
            id: Uuid::new_v4(),
            rule_id: "TEST".into(),
            severity,
            message: "Test finding".into(),
            location: if has_location {
                Some(Location {
                    file: "src/main.rs".into(),
                    line_start: 10,
                    line_end: 12,
                    column_start: 1,
                    column_end: 5,
                })
            } else {
                None
            },
            evidence: "evidence text".into(),
        }
    }

    #[test]
    fn test_zero_findings() {
        let sarif = SarifFormatter::to_string(&[]).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&sarif).unwrap();
        assert_eq!(parsed["version"], "2.1.0");
        assert_eq!(parsed["runs"][0]["results"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn test_single_finding_with_location() {
        let finding = make_finding(Severity::Error, true);
        let sarif = SarifFormatter::to_string(&[finding]).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&sarif).unwrap();
        let results = parsed["runs"][0]["results"].as_array().unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["level"], "error");
        assert_eq!(
            results[0]["locations"][0]["physicalLocation"]["artifactLocation"]["uri"],
            "src/main.rs"
        );
        assert_eq!(
            results[0]["locations"][0]["physicalLocation"]["region"]["startLine"],
            10
        );
    }

    #[test]
    fn test_single_finding_without_location() {
        let finding = make_finding(Severity::Warning, false);
        let sarif = SarifFormatter::to_string(&[finding]).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&sarif).unwrap();
        let results = parsed["runs"][0]["results"].as_array().unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["level"], "warning");
        // locations should not be present (not null, not empty array)
        assert!(results[0].get("locations").is_none());
    }

    #[test]
    fn test_multiple_findings() {
        let findings = vec![
            make_finding(Severity::Error, true),
            make_finding(Severity::Warning, true),
            make_finding(Severity::Info, true),
            make_finding(Severity::Hint, false),
        ];
        let sarif = SarifFormatter::to_string(&findings).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&sarif).unwrap();
        let results = parsed["runs"][0]["results"].as_array().unwrap();
        assert_eq!(results.len(), 4);
        assert_eq!(results[0]["level"], "error");
        assert_eq!(results[1]["level"], "warning");
        assert_eq!(results[2]["level"], "note");
        assert_eq!(results[3]["level"], "none");
    }

    #[test]
    fn test_severity_mapping() {
        assert_eq!(SarifFormatter::severity_to_level(&Severity::Error), "error");
        assert_eq!(
            SarifFormatter::severity_to_level(&Severity::Warning),
            "warning"
        );
        assert_eq!(SarifFormatter::severity_to_level(&Severity::Info), "note");
        assert_eq!(SarifFormatter::severity_to_level(&Severity::Hint), "none");
    }

    #[test]
    fn test_write_to_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("output.sarif");
        let finding = make_finding(Severity::Error, true);
        SarifFormatter::write_to_file(&[finding], &path).unwrap();
        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("Clausura"));
    }
}
