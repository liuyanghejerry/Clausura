//! Request middleware (logging, auth header validation).

/// Extract the bearer token from a request's header lines.
pub fn bearer_token(request: &str) -> Option<String> {
    for line in request.lines() {
        if let Some(rest) = line.strip_prefix("Authorization: Bearer ") {
            if !rest.trim().is_empty() {
                return Some(rest.trim().to_string());
            }
        }
    }
    None
}

/// Validate that a token is non-empty and has the expected prefix.
pub fn validate_token(token: &str) -> bool {
    token.len() >= 16 && (token.starts_with("svc_") || token.starts_with("dev_"))
}
