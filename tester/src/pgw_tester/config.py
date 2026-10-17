"""Settings, read from the environment once at start-up."""

import os
from dataclasses import dataclass, field
from pathlib import Path
from urllib.parse import urlparse

from .load.agents import AgentRef, parse_agents

#: The token shared with the load agents is at least this long.
MIN_LOAD_TOKEN = 32


def _origin(name: str, default: str = "") -> str:
    value = os.environ.get(name, default).strip()
    if not value:
        return ""
    parsed = urlparse(value)
    if parsed.scheme not in ("http", "https") or not parsed.netloc:
        raise ValueError(f"{name} must be a URL such as http://core:8081, got {value!r}")
    return value.rstrip("/")


def _secret(name: str) -> str:
    """A secret from `NAME`, or from the file `NAME_FILE` names (a mounted secret); both set is
    refused, so that it is clear which one is used."""
    value = os.environ.get(name, "").strip()
    path = os.environ.get(f"{name}_FILE", "").strip()
    if value and path:
        raise ValueError(f"{name} and {name}_FILE are both set; set one of them")
    if path:
        return Path(path).read_text().strip()
    return value


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
    #: with the token the console forwarded .
    id_internal_url: str = ""
    id_issuer: str = ""
    id_client_id: str = "pacod-gw-tester"
    id_client_secret: str = ""
    #: PGW_AUTH, as the core has it: only with "oidc" are there grants to take.
    auth: str = "local"
    #: The load agents (`name=URL,…`) and the token they share with the tester.
    load_agents: tuple[AgentRef, ...] = ()
    load_token: str = field(default="", repr=False)
    #: The mock provider for load tests, as the gateway reaches it. Empty: the target "mock"
    #: is not available.
    load_mock_url: str = ""
    #: The most calls a live load test may make, and the longest a load test may last.
    load_live_max_calls: int = 1000
    load_max_duration: int = 3600
    keep_load_runs: int = 50

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
            load_agents=tuple(parse_agents(os.environ.get("TESTER_LOAD_AGENTS", ""))),
            load_token=_secret("TESTER_LOAD_TOKEN"),
            load_mock_url=_origin("TESTER_LOAD_MOCK_URL") or mock,
            load_live_max_calls=int(os.environ.get("TESTER_LOAD_LIVE_MAX_CALLS", "1000")),
            load_max_duration=int(os.environ.get("TESTER_LOAD_MAX_DURATION", "3600")),
            keep_load_runs=int(os.environ.get("TESTER_KEEP_LOAD_RUNS", "50")),
        )
        settings.check()
        return settings

    def check(self) -> None:
        """Refuses settings a run would fail with later: in OIDC mode a run without its grant
        would use the console's token, which ends after five minutes."""
        if self.auth not in ("local", "oidc"):
            raise ValueError(f"PGW_AUTH must be local or oidc, got {self.auth!r}")
        if self.load_agents and len(self.load_token) < MIN_LOAD_TOKEN:
            raise ValueError(
                f"TESTER_LOAD_AGENTS needs TESTER_LOAD_TOKEN (or TESTER_LOAD_TOKEN_FILE) of at least "
                f"{MIN_LOAD_TOKEN} characters, the agents' LOAD_AGENT_TOKEN"
            )
        if self.load_live_max_calls < 1 or self.load_max_duration < 1:
            raise ValueError("TESTER_LOAD_LIVE_MAX_CALLS and TESTER_LOAD_MAX_DURATION are at least 1")
        if self.auth == "oidc" and not self.delegated:
            raise ValueError(
                "PGW_AUTH=oidc needs PGW_ID_INTERNAL_URL and TESTER_ID_CLIENT_SECRET "
                "(the tester's client at pacod-id, pacod-gw-tester)"
            )
