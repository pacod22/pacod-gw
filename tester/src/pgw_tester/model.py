"""The format of a test suite file. See docs/testing.md for the reference."""

import re
from typing import Annotated, Any, Literal

from pydantic import BaseModel, BeforeValidator, ConfigDict, Field, model_validator

Mode = Literal["mock", "live"]
#: What a case or step can depend on: a model in one of the slots, or a mock it can script.
#: `openai` is a model on the OpenAI-compatible wire (OpenAI, DeepSeek, GLM, Kimi, Ollama,
#: Gemini's compatible endpoint), `anthropic` an Anthropic model, `gemini` a model on Gemini's
#: own API.
Need = Literal["openai", "anthropic", "gemini", "mock_control"]

SLUG = r"^[a-z0-9][a-z0-9-]{0,62}$"
_DURATION = re.compile(r"^\s*(\d+(?:\.\d+)?)\s*(ms|s|m)?\s*$")


def parse_duration(value: Any) -> float:
    """Seconds, from a number of seconds or a text such as `200ms`, `2s` or `1m`."""
    if isinstance(value, bool):
        raise ValueError("a duration is a number of seconds or a text such as 200ms, 2s or 1m")
    if isinstance(value, int | float):
        seconds = float(value)
    else:
        match = _DURATION.match(str(value))
        if not match:
            raise ValueError(f"{value!r} is not a duration such as 200ms, 2s or 1m")
        seconds = float(match[1]) * {"ms": 0.001, "s": 1.0, "m": 60.0}[match[2] or "s"]
    if seconds < 0 or seconds > 600:
        raise ValueError("a duration lies between 0 and 10 minutes")
    return seconds


Duration = Annotated[float, BeforeValidator(parse_duration)]


class Strict(BaseModel):
    model_config = ConfigDict(extra="forbid", populate_by_name=True)


class Request(Strict):
    method: str = "GET"
    path: str
    #: `api` puts the path below /api/v1 of the management API; `root` uses it as given
    #: (for /readyz). Only for `mgmt` steps.
    base: Literal["api", "root"] = "api"
    query: dict[str, Any] = {}
    headers: dict[str, str] = {}
    body: Any = None
    #: How the request authenticates. Defaults: `session` for mgmt, `bearer` for call,
    #: `none` for mock.
    auth: Literal["session", "bearer", "x-api-key", "none"] | None = None
    #: The secret for `bearer` and `x-api-key`.
    key: str = "{{key}}"
    timeout: Duration = 60.0

    @model_validator(mode="after")
    def _method(self) -> "Request":
        self.method = self.method.upper()
        if self.method not in {"GET", "POST", "PUT", "PATCH", "DELETE", "HEAD"}:
            raise ValueError(f"unknown method {self.method}")
        if not self.path.startswith("/"):
            raise ValueError("a path starts with /")
        return self


class Retry(Strict):
    #: Retry the step until its expectations hold, for at most this long.
    within: Duration
    every: Duration = 0.2


class Expect(Strict):
    #: The status, or a list of accepted ones. Without it a status below 300 is expected.
    status: int | list[int] | None = None
    headers: dict[str, Any] = {}
    json_paths: dict[str, Any] = Field(default={}, alias="json")
    #: A check on the response text.
    body: Any = None
    #: Checks on the JSON of the last `data:` line of an event stream.
    sse_last: dict[str, Any] = {}
    #: Event names that must appear in an event stream.
    sse_events: list[str] = []


class Step(Strict):
    name: str | None = None
    mgmt: Request | None = None
    call: Request | None = None
    mock: Request | None = None
    sleep: Duration | None = None
    expect: Expect = Expect()
    #: Variables taken from the response: `json.<path>`, `header.<name>` or `status`.
    save: dict[str, str] = {}
    retry: Retry | None = None
    #: The step is skipped unless these are available.
    needs: list[Need] = []
    #: Delete what a mgmt POST created when the suite is torn down.
    cleanup: bool = True

    @model_validator(mode="after")
    def _one_kind(self) -> "Step":
        kinds = [k for k in ("mgmt", "call", "mock", "sleep") if getattr(self, k) is not None]
        if len(kinds) != 1:
            raise ValueError("a step has exactly one of mgmt, call, mock or sleep")
        # The admin's session goes to the management API and nowhere else.
        if kinds[0] != "mgmt" and self.request is not None and self.request.auth == "session":
            raise ValueError("auth: session is for mgmt steps only")
        for name in self.save:
            if not re.match(r"^[A-Za-z_][\w]*$", name):
                raise ValueError(f"{name!r} is not a variable name")
        return self

    @property
    def kind(self) -> str:
        return next(k for k in ("mgmt", "call", "mock", "sleep") if getattr(self, k) is not None)

    @property
    def request(self) -> Request | None:
        return self.mgmt or self.call or self.mock

    def label(self) -> str:
        if self.name:
            return self.name
        if self.sleep is not None:
            return f"sleep {self.sleep:g} s"
        r = self.request
        assert r is not None
        return f"{self.kind}: {r.method} {r.path}"


class Case(Strict):
    id: str = Field(pattern=SLUG)
    title: str
    description: str = ""
    modes: list[Mode] = ["mock", "live"]
    needs: list[Need] = []
    steps: list[Step] = Field(min_length=1)


class Suite(Strict):
    suite: str = Field(pattern=SLUG)
    title: str
    description: str = ""
    #: `scratch` gives the suite an organisation, workspace and key of its own, and the
    #: models of the run's mode granted to it; everything is removed afterwards.
    fixture: Literal["none", "scratch"] = "scratch"
    setup: list[Step] = []
    cases: list[Case] = Field(min_length=1)

    @model_validator(mode="after")
    def _unique_cases(self) -> "Suite":
        ids = [c.id for c in self.cases]
        dupes = sorted({i for i in ids if ids.count(i) > 1})
        if dupes:
            raise ValueError(f"case ids appear more than once: {', '.join(dupes)}")
        return self
