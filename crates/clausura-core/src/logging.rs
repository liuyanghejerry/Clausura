use crate::config::LogFormat;
use tracing_subscriber::prelude::*;

/// Initialize logging with the specified format and level.
///
/// Log level is controlled by the `CLAUSURA_LOG` environment variable
/// (defaults to `info`). All log output goes to stderr; stdout is reserved
/// for SARIF output.
pub fn init(format: &LogFormat) {
    let level = std::env::var("CLAUSURA_LOG").unwrap_or_else(|_| "info".to_string());

    let env_filter = tracing_subscriber::EnvFilter::builder().parse_lossy(&level);

    let json_layer = tracing_subscriber::fmt::layer()
        .json()
        .with_target(true)
        .with_current_span(true)
        .with_span_list(true)
        .with_writer(std::io::stderr)
        .with_timer(tracing_subscriber::fmt::time::UtcTime::rfc_3339());

    let pretty_layer = tracing_subscriber::fmt::layer()
        .pretty()
        .with_target(true)
        .with_writer(std::io::stderr);

    let subscriber = tracing_subscriber::registry().with(env_filter);

    match format {
        LogFormat::Json => {
            subscriber.with(json_layer).init();
        }
        LogFormat::Pretty => {
            subscriber.with(pretty_layer).init();
        }
    }
}

/// Mask sensitive information in log messages.
///
/// Currently masks OpenAI-style API keys: `sk-...abc123def` becomes `sk-...***`.
/// This is intended to be called before logging sensitive values so that the
/// masked string is what gets emitted by the tracing subscriber.
pub fn mask_sensitive(input: &str) -> String {
    let re = regex_lite::Regex::new(r"(?i)(sk-[a-zA-Z0-9]{20,})").unwrap();
    let result = re.replace_all(input, |caps: &regex_lite::Captures| {
        let key = &caps[1];
        if key.len() > 10 {
            format!("{}...***", &key[..10])
        } else {
            "***".to_string()
        }
    });
    result.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mask_openai_key() {
        let input = "sk-abc123def456ghi789jkl012";
        let masked = mask_sensitive(input);
        assert_eq!(masked, "sk-abc123d...***");
        assert!(!masked.contains("jkl012"));
        assert!(masked.ends_with("***"));
    }

    #[test]
    fn test_mask_no_key() {
        let input = "Just a normal log message";
        let masked = mask_sensitive(input);
        assert_eq!(masked, input);
    }

    #[test]
    fn test_mask_partial_key() {
        // Very short key-like string doesn't match the 20-char minimum
        let input = "sk-abc";
        let masked = mask_sensitive(input);
        assert_eq!(masked, "sk-abc");
    }

    #[test]
    fn test_mask_empty_string() {
        let input = "";
        let masked = mask_sensitive(input);
        assert_eq!(masked, "");
    }

    #[test]
    fn test_mask_key_in_context() {
        let input = "Using API key sk-abc123def456ghi789jkl012 for provider.openai";
        let masked = mask_sensitive(input);
        assert_eq!(masked, "Using API key sk-abc123d...*** for provider.openai");
        assert!(!masked.contains("jkl012"));
    }
}
