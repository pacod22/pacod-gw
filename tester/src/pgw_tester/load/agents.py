"""The load agents the tester knows (`TESTER_LOAD_AGENTS`), and how it talks to one."""

import re
from dataclasses import dataclass
from typing import Any
from urllib.parse import urlparse

import httpx
from pydantic import ValidationError

from .contract import Info, Prepare, Prepared, Report

_NAME = re.compile(r"^[a-z0-9][a-z0-9-]{0,31}$")


@dataclass(frozen=True)
class AgentRef:
    name: str
    url: str


def parse_agents(text: str) -> list[AgentRef]:
    """`local=http://load-agent:8096,remote=http://10.0.0.8:8096`."""
    out: list[AgentRef] = []
    for item in (text or "").split(","):
        item = item.strip()
        if not item:
            continue
        name, sep, url = item.partition("=")
        name, url = name.strip(), url.strip().rstrip("/")
        if not sep or not _NAME.match(name):
            raise ValueError(f"TESTER_LOAD_AGENTS: {item!r} is not name=URL (a lower-case name, at most 32)")
        parsed = urlparse(url)
        if parsed.scheme not in ("http", "https") or not parsed.hostname:
            raise ValueError(f"TESTER_LOAD_AGENTS: the URL of {name} is not http(s) with a host")
        if any(a.name == name for a in out):
            raise ValueError(f"TESTER_LOAD_AGENTS: {name} is named twice")
        out.append(AgentRef(name, url))
    return out


class AgentError(Exception):
    """An agent did not answer, or refused: one sentence, without the token."""

    def __init__(self, agent: str, message: str, status: int | None = None, code: str = ""):
        super().__init__(f"agent {agent}: {message}")
        self.status = status
        self.code = code


class AgentClient:
    def __init__(self, ref: AgentRef, token: str, http: httpx.AsyncClient):
        self.ref = ref
        self.name = ref.name
        self._token = token
        self._http = http

    async def _call(self, method: str, path: str, timeout: float, body: Any = None) -> Any:
        try:
            r = await self._http.request(
                method,
                f"{self.ref.url}{path}",
                headers={"authorization": f"Bearer {self._token}"},
                json=body,
                timeout=timeout,
            )
        except httpx.HTTPError as e:
            raise AgentError(self.name, f"does not answer at {self.ref.url} ({type(e).__name__})") from None
        try:
            data = r.json()
        except ValueError:
            data = None
        if r.status_code >= 400:
            detail = data.get("error", {}) if isinstance(data, dict) else {}
            message = detail.get("message") or f"answered {r.status_code}"
            if r.status_code == 401:
                message = (
                    "refused the tester's token (TESTER_LOAD_TOKEN and the agent's LOAD_AGENT_TOKEN differ)"
                )
            raise AgentError(self.name, message, r.status_code, detail.get("code", ""))
        return data

    async def info(self) -> Info:
        return self._parse(Info, await self._call("GET", "/v1/info", 3))

    async def prepare(self, plan: Prepare, timeout: float) -> Prepared:
        return self._parse(Prepared, await self._call("POST", "/v1/runs", timeout, plan.wire()))

    async def start(self, run_id: str) -> None:
        await self._call("POST", f"/v1/runs/{run_id}/start", 5)

    async def report(self, run_id: str, after: int | None) -> Report:
        path = f"/v1/runs/{run_id}/report" + (f"?after={after}" if after is not None and after >= 0 else "")
        return self._parse(Report, await self._call("GET", path, 5))

    async def cancel(self, run_id: str) -> None:
        await self._call("POST", f"/v1/runs/{run_id}/cancel", 5)

    def _parse[T](self, model: type[T], data: Any) -> T:
        try:
            return model.model_validate(data)  # type: ignore[attr-defined]
        except ValidationError:
            raise AgentError(
                self.name, "answered in a form this tester does not read; are both of one version?"
            ) from None
