"""A load test as the console asks for it, and the run it records.

Everything in a run is stored and shown, so secrets never get into one: a pasted key is held in
memory for the run, and the run keeps only a hint of it.
"""

import re
from datetime import datetime
from typing import Any, Literal
from urllib.parse import urlparse

from pydantic import BaseModel, Field, field_validator, model_validator

from ..results import Starter, StepResult, now
from ..store import JsonStore
from .contract import Errors, Probe, Stage

Target = Literal["mock", "live", "own", "external"]
Interface = Literal["openai", "anthropic", "gemini", "native", "embeddings"]
LoadStatus = Literal[
    "queued", "preparing", "running", "draining", "cleaning", "finished", "cancelled", "error", "interrupted"
]
LOAD_FINISHED: set[str] = {"finished", "cancelled", "error", "interrupted"}

ID = re.compile(r"^[A-Za-z0-9_-]{1,100}$")
AGENT_NAME = re.compile(r"^[a-z0-9][a-z0-9-]{0,31}$")
#: The header option the mock reads (`x-mock`); only proxy mode forwards it.
_MOCK_OPTIONS = re.compile(r"^[A-Za-z0-9_=,.-]{0,500}$")


def check_base_url(value: str) -> str:
    """An external gateway: http(s), a host, no user name or password, query or fragment. The
    agent checks the same; checking here too answers the console before any agent is asked."""
    value = value.strip().rstrip("/")
    parsed = urlparse(value)
    if parsed.scheme not in ("http", "https") or not parsed.hostname:
        raise ValueError("the gateway's URL is http:// or https:// with a host")
    if parsed.username or parsed.password:
        raise ValueError("the gateway's URL may not hold a user name or password")
    if parsed.query or parsed.fragment:
        raise ValueError("the gateway's URL may not hold a query or a fragment")
    if any(c.isspace() for c in value):
        raise ValueError("the gateway's URL may not hold spaces")
    return value


class MockOptions(BaseModel):
    #: The mock provider behind the gateway: its API decides the dialect the gateway speaks
    #: upstream.
    family: Literal["openai", "anthropic", "gemini"] = "openai"
    delay_ms: int = Field(default=200, ge=0, le=60_000)
    chunk_delay_ms: int = Field(default=20, ge=0, le=10_000)
    chunks: int = Field(default=20, ge=1, le=1000)

    def spec(self) -> str:
        return f"delay={self.delay_ms},chunk_delay={self.chunk_delay_ms},chunks={self.chunks}"


class LiveOptions(BaseModel):
    model_id: str

    @field_validator("model_id")
    @classmethod
    def _id(cls, value: str) -> str:
        if not ID.match(value):
            raise ValueError("not a model id")
        return value


class KeyedOptions(BaseModel):
    """A key the admin pastes: of a workspace on this gateway (`own`) or of another gateway
    (`external`). Held for the run, never stored."""

    secret: str = Field(repr=False, min_length=1, max_length=4096)
    #: The model as the gateway names it (`openai/gpt-dev`, a pool `@fast`).
    model: str = Field(min_length=1, max_length=200)
    #: Proxy mode: the provider in `/proxy/<provider>/…`, and the model's id there.
    provider: str = Field(default="", max_length=100)
    upstream: str = Field(default="", max_length=200)
    #: External only: the gateway as the agents reach it.
    base_url: str = ""
    #: Proxy mode: options for a mock provider behind the gateway (`delay=200,chunks=8`), sent
    #: as `x-mock`, which proxy mode forwards.
    mock_options: str = ""

    @field_validator("secret")
    @classmethod
    def _secret(cls, value: str) -> str:
        value = value.strip()
        if not value or any(ord(c) < 32 or ord(c) == 127 for c in value):
            raise ValueError("the key is not a valid header value")
        return value

    @field_validator("provider")
    @classmethod
    def _provider(cls, value: str) -> str:
        value = value.strip()
        if value and not ID.match(value):
            raise ValueError("not a provider name")
        return value

    @field_validator("mock_options")
    @classmethod
    def _mock_options(cls, value: str) -> str:
        value = value.strip()
        if not _MOCK_OPTIONS.match(value):
            raise ValueError("mock options are name=value pairs separated by commas")
        return value


class LoadShape(BaseModel):
    mode: Literal["concurrency", "rate"] = "concurrency"
    stages: list[Stage] = Field(min_length=1, max_length=50)
    #: Rate mode: calls running at once above which arrivals are skipped.
    max_in_flight: int | None = Field(default=None, ge=1)
    timeout_s: int = Field(default=120, ge=1, le=600)
    #: Every call the run makes, the probes and the sanity call included.
    max_calls: int | None = Field(default=None, ge=1)

    @model_validator(mode="after")
    def _shape(self) -> "LoadShape":
        if sum(s.duration_s for s in self.stages) <= 0:
            raise ValueError("the stages last no time at all")
        if self.mode == "rate" and self.max_in_flight is None:
            raise ValueError("rate mode needs max_in_flight")
        return self

    def duration_s(self) -> int:
        return sum(s.duration_s for s in self.stages)

    def peak(self) -> float:
        return max(s.target for s in self.stages)


class LoadRequest(BaseModel):
    label: str = Field(default="", max_length=80)
    target: Target
    interface: Interface = "openai"
    mode: Literal["managed", "proxy"] = "managed"
    mock: MockOptions | None = None
    live: LiveOptions | None = None
    own: KeyedOptions | None = None
    external: KeyedOptions | None = None
    stream: bool = True
    prompt_words: int = Field(default=60, ge=1, le=20_000)
    max_tokens: int = Field(default=64, ge=1, le=100_000)
    load: LoadShape
    agents: list[str] = Field(min_length=1, max_length=50)
    http: Literal["1.1", "2"] = "1.1"
    acknowledge_cost: bool = False
    keep_resources: bool = False

    @model_validator(mode="after")
    def _consistent(self) -> "LoadRequest":
        options = {"mock": self.mock, "live": self.live, "own": self.own, "external": self.external}
        if options[self.target] is None:
            raise ValueError(f"the {self.target} target needs its options")
        if len(set(self.agents)) != len(self.agents):
            raise ValueError("an agent is named twice")
        if self.mode == "proxy" and self.interface == "native":
            raise ValueError("the native interface exists in managed mode only")
        if self.interface == "embeddings":
            self.stream = False
        if self.target == "mock" and self.mock is not None:
            if self.interface == "embeddings" and self.mock.family != "openai":
                raise ValueError("embeddings come from the OpenAI mock")
            if self.mode == "proxy" and _proxy_dialect(self.interface) != self.mock.family:
                raise ValueError("in proxy mode the interface is the mock provider's own API")
        keyed = self.own if self.target == "own" else self.external if self.target == "external" else None
        if keyed is not None:
            if self.mode == "proxy" and not (keyed.provider and keyed.upstream):
                raise ValueError("proxy mode needs the provider's name and the model's id there")
            if self.target == "external":
                keyed.base_url = check_base_url(keyed.base_url)
        return self

    def public(self) -> dict[str, Any]:
        """The request as a run keeps it: without the pasted key."""
        data = self.model_dump(mode="json")
        for name in ("own", "external"):
            if data.get(name):
                secret = data[name].pop("secret", "")
                data[name]["key_hint"] = key_hint(secret)
        return data


def _proxy_dialect(interface: str) -> str:
    return "openai" if interface == "embeddings" else interface


def key_hint(secret: str) -> str:
    """What a run says about a pasted key: a gateway key's id part (`pgw_<id>_…`), which is not
    secret, or the start of its SHA-256."""
    import hashlib

    parts = secret.split("_")
    if len(parts) >= 3 and parts[0] == "pgw" and parts[1]:
        return f"pgw_{parts[1]}_…"
    return "sha256:" + hashlib.sha256(secret.encode()).hexdigest()[:8]


class Share(BaseModel):
    """One agent's part of the load."""

    stages: list[Stage]
    max_in_flight: int | None = None
    max_calls: int | None = None


class AgentPart(BaseModel):
    name: str
    url: str
    version: str = ""
    #: The agent's own route to this gateway; external runs go to the request's URL instead.
    target: str = ""
    cpus: int = 0
    share: Share | None = None
    state: str = "pending"
    ended: str | None = None
    error: str = ""
    probe: Probe | None = None
    calls: int = 0
    #: The agent's highest CPU use, in percent of all its CPUs.
    cpu_max: float = 0
    lag_max_ms: float = 0
    last_t: int = -1


class Scene(BaseModel):
    """What the tester set up for the run, and took down after it."""

    org: str = ""
    ws: str = ""
    key_id: str = ""
    model: str = ""
    model_id: str = ""
    provider: str = ""
    upstream: str = ""
    #: Mock: the latency profile in the provider's URLs.
    profile: str = ""
    #: Paths of the objects made for the run, deleted after it.
    ledger: list[str] = []
    setup: list[StepResult] = []
    teardown: list[StepResult] = []
    leftovers: list[str] = []


class Pct(BaseModel):
    """Milliseconds."""

    p50: float = 0
    p95: float = 0
    p99: float = 0
    max: float = 0


class Point(BaseModel):
    """One second of the run, all agents together."""

    t: int
    #: Calls at a time, or a second, that the agents aimed at.
    target: float = 0
    started: int = 0
    completed: int = 0
    ok: int = 0
    #: Calls without a complete answer, and answers that were not a success.
    failed: int = 0
    cancelled: int = 0
    status: dict[str, int] = {}
    bytes: int = 0
    in_flight: int = 0
    skipped: int = 0
    ttfb: Pct | None = None
    total: Pct | None = None
    gw_delay: Pct | None = None
    #: The busiest agent's CPU use, in percent of its CPUs, and the worst scheduler lag.
    cpu_max: float | None = None
    lag_max_ms: float = 0
    #: Calls in flight at the gateway (all instances), from its own figures.
    gw_in_flight: int | None = None
    #: Agents that reported this second.
    agents: int = 0


class GatewaySample(BaseModel):
    t: float
    in_flight: int = 0
    requests_per_second: float = 0
    rejected_per_second: float = 0
    spool_bytes: int = 0
    events_dropped: int = 0


class Summary(BaseModel):
    duration_s: float = 0
    started: int = 0
    calls: int = 0
    ok: int = 0
    failed: int = 0
    cancelled: int = 0
    skipped: int = 0
    status: dict[str, int] = {}
    errors: Errors = Errors()
    bytes: int = 0
    #: Calls finished a second, on average and in the busiest second.
    rps_mean: float = 0
    rps_peak: float = 0
    ttfb: Pct = Pct()
    total: Pct = Pct()
    gw_delay: Pct | None = None


class LoadRun(BaseModel):
    id: str
    status: LoadStatus = "queued"
    label: str = ""
    target: Target
    started_by: Starter
    created_at: datetime = Field(default_factory=now)
    started_at: datetime | None = None
    finished_at: datetime | None = None
    #: The request without secrets (`LoadRequest.public`).
    request: dict[str, Any] = {}
    #: Where the calls went: this gateway (each agent's own route to it) or an external URL.
    base_url: str = ""
    agents: list[AgentPart] = []
    scene: Scene = Scene()
    points: list[Point] = []
    gateway: list[GatewaySample] = []
    summary: Summary | None = None
    warnings: list[str] = []
    error: str = ""

    def view(self) -> dict[str, Any]:
        """The run without its seconds and steps, for lists and events."""
        return load_summary(self.model_dump(mode="json"))


def load_summary(data: dict[str, Any]) -> dict[str, Any]:
    data.pop("points", None)
    data.pop("gateway", None)
    scene = data.get("scene") or {}
    data["leftovers"] = len(scene.get("leftovers", []))
    data.pop("scene", None)
    data["agents"] = [{"name": a.get("name"), "state": a.get("state")} for a in data.get("agents", [])]
    return data


class LoadStore(JsonStore[LoadRun]):
    def __init__(self, directory, keep: int = 50):
        super().__init__(directory, LoadRun, load_summary, keep)

    def recover(self) -> None:
        """Runs that were going when the tester stopped are marked as interrupted, with what
        they had set up listed as left over (the agents stop on their own: their lease runs
        out)."""
        for run_id in self.ids():
            run = self.load(run_id)
            if run and run.status not in LOAD_FINISHED:
                run.status = "interrupted"
                run.finished_at = run.finished_at or now()
                run.error = (
                    "the tester stopped during the load test; the agents stopped when their lease ran out"
                )
                if not run.request.get("keep_resources"):
                    done = {
                        s.name.removeprefix("delete ") for s in run.scene.teardown if s.status == "passed"
                    }
                    run.scene.leftovers = [p for p in reversed(run.scene.ledger) if p not in done]
                self.save(run)
