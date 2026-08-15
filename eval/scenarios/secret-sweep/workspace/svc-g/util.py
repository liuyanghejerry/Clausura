"""Service util-svc-g (eval fixture)."""

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
