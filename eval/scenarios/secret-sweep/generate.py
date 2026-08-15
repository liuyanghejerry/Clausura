#!/usr/bin/env python3
"""Generate the secret-sweep fixture: 38 Python files across 10 services.

- 12 files carry REAL seeded secrets (values like `sk-sweep-<n>-...`).
- 8 files carry DECOY placeholders (`REPLACE_ME` / `example-...`) that a
  correct reviewer must NOT report.
- The remaining files are clean boilerplate.

Deterministic (fixed seed). Regenerate with:
    python3 generate.py
from this directory; the output lands in ./workspace.
"""

import json
import os
import random
from pathlib import Path

random.seed(20260815)

ROOT = Path(__file__).parent / "workspace"
SERVICES = [f"svc-{chr(ord('a') + i)}" for i in range(10)]  # svc-a .. svc-j

REAL_SECRETS = [
    "sk-sweep-1f3a9c7e2b4d8a6c",
    "ghp_sweepT0ken9q2w8e4r7t",
    "AKIAEXAMPLE12SWEEP34KEY",
    "sk-sweep-5d8b2f4a6c1e9d3b",
    "ghp_sweepS3cr3t8u7y6t5r4",
    "AKIAEXAMPLE56SWEEP78KEY",
    "sk-sweep-9c1e3d5f7b2a4c6e",
    "ghp_sweepK3y9a8s7d6f5g4",
    "AKIAEXAMPLE90SWEEP12KEY",
    "sk-sweep-2b4d6f8a1c3e5d7b",
    "ghp_sweepT0k3n4e5r6t7y8u",
    "AKIAEXAMPLE34SWEEP56KEY",
]

DECOY_VALUES = [
    '"REPLACE_ME"',
    '"your-api-key-here"',
    '"example-api-key"',
    '"CHANGE_ME_BEFORE_DEPLOY"',
    '"placeholder-token"',
    '"<YOUR_SECRET>"',
    '"dummy-key-1234"',
    '"not-a-real-secret"',
]

BOILERPLATE = '''
"""Service configuration and request handling (fixture boilerplate)."""

import json
import logging
from dataclasses import dataclass, field
from typing import Any

logger = logging.getLogger(__name__)


@dataclass
class Request:
    path: str
    body: dict[str, Any] = field(default_factory=dict)
    headers: dict[str, str] = field(default_factory=dict)


@dataclass
class Response:
    status: int = 200
    payload: dict[str, Any] = field(default_factory=dict)


class Handler:
    def __init__(self, name: str, pool_size: int = 8):
        self.name = name
        self.pool_size = pool_size
        self.metrics: dict[str, int] = {}

    def record(self, key: str, n: int = 1) -> None:
        self.metrics[key] = self.metrics.get(key, 0) + n

    def dispatch(self, request: Request) -> Response:
        self.record(f"dispatch:{request.path}")
        logger.info("handling %s via %s", request.path, self.name)
        return Response(payload={"handled_by": self.name, "path": request.path})

    def drain(self) -> dict[str, int]:
        out = dict(self.metrics)
        self.metrics.clear()
        return out


def build_handler(name: str) -> Handler:
    """Construct a handler with sane defaults."""
    return Handler(name=name, pool_size=16)


def serialize(response: Response) -> str:
    """Render a response to its wire form."""
    return json.dumps(response.payload, sort_keys=True)


def parse_wire(data: str) -> Request:
    """Parse an incoming wire payload."""
    raw = json.loads(data)
    return Request(path=raw.get("path", "/"), body=raw.get("body", {}))
'''


def boilerplate(name: str) -> str:
    return f'"""Service {name} (eval fixture)."""\n' + BOILERPLATE


def secret_file(secret: str) -> str:
    return f'''"""Service configuration (eval fixture)."""

# Deployment configuration — values injected at build time.
# NOTE: this credential is intentionally seeded for the eval scenario.
DEPLOY_CONFIG = {{
    "region": "us-east-1",
    "retries": 3,
    "timeout_secs": 30,
    "api_secret": "{secret}",
}}


def get_config() -> dict:
    """Return the effective deployment configuration."""
    return dict(DEPLOY_CONFIG)
'''


def decoy_file(decoy: str) -> str:
    return f'''"""Service configuration (eval fixture)."""

# Placeholder credential — fill in from the secret store before deploy.
DEPLOY_CONFIG = {{
    "region": "us-west-2",
    "retries": 2,
    "timeout_secs": 15,
    "api_secret": {decoy},
}}


def get_config() -> dict:
    """Return the effective deployment configuration."""
    return dict(DEPLOY_CONFIG)
'''


def main() -> None:
    if ROOT.exists():
        for p in ROOT.iterdir():
            if p.is_dir():
                for f in p.iterdir():
                    f.unlink()
                p.rmdir()
            else:
                p.unlink()
    os.makedirs(ROOT, exist_ok=True)

    # 10 config.py all carry real secrets; 2 util.py carry the last two
    # real secrets. 8 decoy files land as config_extra.py/util_extra.py
    # across the first four services.
    real = iter(REAL_SECRETS)
    decoy = iter(DECOY_VALUES)

    for svc in SERVICES:
        svc_dir = ROOT / svc
        os.makedirs(svc_dir, exist_ok=True)
        (svc_dir / "README.md").write_text(f"# {svc}\nFixture service.\n")

        (svc_dir / "config.py").write_text(secret_file(next(real)))

        if svc in SERVICES[:2]:
            secret = next(real)
            util = f'''"""Shared helpers (eval fixture)."""

# NOTE: seeded credential for the eval scenario.
EXPORT_TOKEN = "{secret}"

def make_headers(extra: dict | None = None) -> dict:
    """Build request headers."""
    headers = {{"Authorization": f"Bearer {{EXPORT_TOKEN}}"}}
    if extra:
        headers.update(extra)
    return headers
'''
            (svc_dir / "util.py").write_text(util)
        else:
            (svc_dir / "util.py").write_text(boilerplate(f"util-{svc}"))

        (svc_dir / "handlers.py").write_text(boilerplate(f"handlers-{svc}"))

    for svc in SERVICES[:4]:
        svc_dir = ROOT / svc
        (svc_dir / "config_extra.py").write_text(decoy_file(next(decoy)))
        (svc_dir / "util_extra.py").write_text(decoy_file(next(decoy)))

    n_files = sum(1 for _ in ROOT.rglob("*.py"))
    print(f"generated {n_files} python files under {ROOT}")


if __name__ == "__main__":
    main()
