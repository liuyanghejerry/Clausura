"""Service configuration (eval fixture)."""

# Placeholder credential — fill in from the secret store before deploy.
DEPLOY_CONFIG = {
    "region": "us-west-2",
    "retries": 2,
    "timeout_secs": 15,
    "api_secret": "REPLACE_ME",
}


def get_config() -> dict:
    """Return the effective deployment configuration."""
    return dict(DEPLOY_CONFIG)
