"""What a run records. Everything here is stored and shown, so it is redacted before it gets here."""

from datetime import UTC, datetime
from typing import Literal

from pydantic import BaseModel, Field

StepStatus = Literal["passed", "failed", "error", "skipped"]
CaseStatus = Literal["pending", "running", "passed", "failed", "error", "skipped"]
RunStatus = Literal["queued", "running", "passed", "failed", "cancelled", "error", "interrupted"]
FINISHED: set[str] = {"passed", "failed", "cancelled", "error", "interrupted"}


def now() -> datetime:
    return datetime.now(UTC)


class Sent(BaseModel):
    method: str
    url: str
    headers: dict[str, str] = {}
    body: str | None = None


class Received(BaseModel):
    status: int
    headers: dict[str, str] = {}
    body: str = ""
    elapsed_ms: float = 0


class StepResult(BaseModel):
    name: str
    kind: str
    status: StepStatus
    #: One line on what happened, shown next to the step.
    detail: str = ""
    failures: list[str] = []
    attempts: int = 0
    duration_ms: float = 0
    request: Sent | None = None
    response: Received | None = None


class CaseResult(BaseModel):
    id: str
    title: str
    status: CaseStatus = "pending"
    reason: str = ""
    duration_ms: float = 0
    steps: list[StepResult] = []


class SuiteResult(BaseModel):
    suite: str
    title: str
    source: str
    status: Literal["pending", "running", "done"] = "pending"
    setup: list[StepResult] = []
    cases: list[CaseResult] = []
    teardown: list[StepResult] = []
    #: Paths that could not be deleted after the suite.
    leftovers: list[str] = []


class Starter(BaseModel):
    id: str
    display: str


class RunParams(BaseModel):
    #: Live mode: the repository ids of an OpenAI-dialect model and an Anthropic model.
    openai_model: str | None = None
    anthropic_model: str | None = None


class RunRequest(BaseModel):
    mode: Literal["mock", "live"]
    #: Case ids; empty means every case of the catalog.
    cases: list[str] = []
    params: RunParams = RunParams()
    stop_on_fail: bool = False
    keep_resources: bool = False


class Counts(BaseModel):
    total: int = 0
    passed: int = 0
    failed: int = 0
    error: int = 0
    skipped: int = 0


class Run(BaseModel):
    id: str
    status: RunStatus = "queued"
    mode: Literal["mock", "live"]
    started_by: Starter
    created_at: datetime = Field(default_factory=now)
    started_at: datetime | None = None
    finished_at: datetime | None = None
    params: RunParams = RunParams()
    stop_on_fail: bool = False
    keep_resources: bool = False
    error: str = ""
    counts: Counts = Counts()
    suites: list[SuiteResult] = []

    def recount(self) -> None:
        c = Counts()
        for s in self.suites:
            for case in s.cases:
                c.total += 1
                if case.status in ("passed", "failed", "error", "skipped"):
                    setattr(c, case.status, getattr(c, case.status) + 1)
        self.counts = c

    def summary(self) -> dict:
        """The run without its suites, for lists and events."""
        data = self.model_dump(mode="json", exclude={"suites"})
        data["leftovers"] = sum(len(s.leftovers) for s in self.suites)
        return data
