"""Settings, read from the environment once at start-up."""

import os
from dataclasses import dataclass
from pathlib import Path
from urllib.parse import urlparse


def _origin(name: str, default: str = "") -> str:
    value = os.environ.get(name, default).strip()
    if not value:
        return ""
    parsed = urlparse(value)
    if parsed.scheme not in ("http", "https") or not parsed.netloc:
        raise ValueError(f"{name} must be a URL such as http://core:8081, got {value!r}")
    return value.rstrip("/")


@dataclass(frozen=True)
class Settings:
    #: The management listener of the core, without /api/v1.
    admin_url: str
    #: The data plane of the core.
    data_url: str
    #: The mock provider as the gateway reaches it. Empty: mock mode is not available.
    mock_url: str
    #: The mock provider as the tester reaches it, to script failures.
    mock_control_url: str
    #: Where runs and uploaded cases are kept.
    data_dir: Path
    #: How many runs the history keeps.
    keep_runs: int = 100

    @classmethod
    def from_env(cls) -> "Settings":
        mock = _origin("TESTER_MOCK_URL")
        return cls(
            admin_url=_origin("PGW_CORE_ADMIN_URL", "http://127.0.0.1:28081"),
            data_url=_origin("PGW_CORE_DATA_URL", "http://127.0.0.1:28080"),
            mock_url=mock,
            mock_control_url=_origin("TESTER_MOCK_CONTROL_URL", mock) if mock else "",
            data_dir=Path(os.environ.get("TESTER_DATA_DIR", "/data")),
            keep_runs=int(os.environ.get("TESTER_KEEP_RUNS", "100")),
        )
