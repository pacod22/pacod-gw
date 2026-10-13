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
    #: pacod-id's internal listener and the tester's client there, when the gateway signs people
    #: in through pacod-id: a run then works with a delegated grant of the admin's session, not
    #: with the token the console forwarded (identity concept 5.1).
    id_internal_url: str = ""
    id_issuer: str = ""
    id_client_id: str = "pacod-gw-tester"
    id_client_secret: str = ""
    #: PGW_AUTH, as the core has it: only with "oidc" are there grants to take.
    auth: str = "local"

    @property
    def delegated(self) -> bool:
        return self.auth == "oidc" and bool(self.id_internal_url and self.id_client_secret)

    @classmethod
    def from_env(cls) -> "Settings":
        mock = _origin("TESTER_MOCK_URL")
        settings = cls(
            admin_url=_origin("PGW_CORE_ADMIN_URL", "http://127.0.0.1:28081"),
            data_url=_origin("PGW_CORE_DATA_URL", "http://127.0.0.1:28080"),
            mock_url=mock,
            mock_control_url=_origin("TESTER_MOCK_CONTROL_URL", mock) if mock else "",
            data_dir=Path(os.environ.get("TESTER_DATA_DIR", "/data")),
            keep_runs=int(os.environ.get("TESTER_KEEP_RUNS", "100")),
            id_internal_url=_origin("PGW_ID_INTERNAL_URL"),
            id_issuer=_origin("PGW_ID_ISSUER"),
            id_client_id=os.environ.get("TESTER_ID_CLIENT_ID", "").strip() or "pacod-gw-tester",
            id_client_secret=os.environ.get("TESTER_ID_CLIENT_SECRET", "").strip(),
            auth=os.environ.get("PGW_AUTH", "").strip() or "local",
        )
        settings.check()
        return settings

    def check(self) -> None:
        """Refuses settings a run would fail with later: in OIDC mode a run without its grant
        would use the console's token, which ends after five minutes."""
        if self.auth not in ("local", "oidc"):
            raise ValueError(f"PGW_AUTH must be local or oidc, got {self.auth!r}")
        if self.auth == "oidc" and not self.delegated:
            raise ValueError(
                "PGW_AUTH=oidc needs PGW_ID_INTERNAL_URL and TESTER_ID_CLIENT_SECRET "
                "(the tester's client at pacod-id, pacod-gw-tester)"
            )
