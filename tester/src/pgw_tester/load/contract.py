"""The agents' JSON, as pydantic models: `crates/pacod-gw-load/src/contract.rs` on the
tester's side. The shared fixtures keep the two in step."""

from typing import Any, Literal

from pydantic import BaseModel, Field

CONTRACT = 1

AuthHeader = Literal["authorization", "x-api-key", "x-goog-api-key"]
AgentState = Literal["prepared", "running", "draining", "done", "cancelled", "failed"]
Ended = Literal["duration", "max_calls", "cancelled", "lease_expired", "error"]
FINISHED_STATES = {"done", "cancelled", "failed"}


class Caps(BaseModel):
    max_concurrency: int
    max_rate: float
    max_duration_s: int
    max_body_bytes: int


class AgentBusy(BaseModel):
    run_id: str
    state: AgentState


class Info(BaseModel):
    contract: int
    name: str
    version: str
    target: str
    caps: Caps
    cpus: int
    busy: AgentBusy | None = None


class RequestSpec(BaseModel):
    method: str
    path: str
    headers: dict[str, str] = {}
    body: str = ""
    stream: bool = False


class Stage(BaseModel):
    duration_s: int = Field(ge=0)
    target: float = Field(ge=0)


class Profile(BaseModel):
    mode: Literal["concurrency", "rate"]
    stages: list[Stage]
    max_in_flight: int | None = None
    timeout_s: int
    max_calls: int | None = None
    drain_s: int = 30
    http: Literal["1.1", "2"] = "1.1"
    warmup_connections: int = 0


class Prepare(BaseModel):
    contract: int = CONTRACT
    run_id: str
    part: int
    parts: int
    key: str = Field(repr=False)
    auth_header: AuthHeader
    base_url: str | None = None
    request: RequestSpec
    profile: Profile
    lease_s: int = 30

    def wire(self) -> dict[str, Any]:
        """The JSON the agent reads: without `base_url` when it is not set."""
        data = self.model_dump(mode="json")
        if data.get("base_url") is None:
            data.pop("base_url", None)
        return data


class Probe(BaseModel):
    status: int | None = None
    ttfb_ms: float = 0
    total_ms: float = 0
    error: str | None = None
    body_excerpt: str = ""


class Prepared(BaseModel):
    run_id: str
    state: AgentState
    probe: Probe


class Errors(BaseModel):
    connect: int = 0
    timeout: int = 0
    body: int = 0
    other: int = 0
    cancelled: int = 0

    def failed(self) -> int:
        return self.connect + self.timeout + self.body + self.other


class Lag(BaseModel):
    max: float = 0
    mean: float = 0


class Hist(BaseModel):
    n: int = 0
    min: int = 0
    max: int = 0
    sum: int = 0
    b: list[tuple[int, int]] = []


class Interval(BaseModel):
    t: int
    target: float = 0
    started: int = 0
    completed: int = 0
    ok: int = 0
    errors: Errors = Errors()
    status: dict[int, int] = {}
    bytes: int = 0
    in_flight: int = 0
    skipped: int = 0
    lag_ms: Lag = Lag()
    cpu_pct: float | None = None
    ttfb: Hist = Hist()
    total: Hist = Hist()
    gw_delay: Hist = Hist()


class Report(BaseModel):
    run_id: str
    state: AgentState
    ended: Ended | None = None
    error: str = ""
    elapsed_ms: int = 0
    in_flight: int = 0
    calls: int = 0
    intervals: list[Interval] = []


class ErrorDetail(BaseModel):
    code: str
    message: str


class ErrorBody(BaseModel):
    error: ErrorDetail
