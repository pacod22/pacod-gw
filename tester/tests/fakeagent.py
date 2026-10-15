"""A fake load agent: the contract of `pacod-gw-load-agent`, with a scripted run."""

import json

import httpx

from pgw_tester.load.histogram import index

LOAD_TOKEN = "test-only-load-token-0123456789abcdef"


def hist(us: int, n: int) -> dict:
    return {"n": n, "min": us, "max": us, "sum": us * n, "b": [[index(us), n]] if n else []}


class FakeAgent:
    def __init__(self, name: str, seconds: int = 3, per_second: int = 10):
        self.name = name
        self.host = f"agent-{name}"
        self.seconds = seconds
        self.per_second = per_second
        #: Seconds keep coming until cancelled.
        self.endless = False
        self.unreachable = False
        #: Reports answered before the agent stops answering.
        self.answers: int | None = None
        self.busy: dict | None = None
        self.contract = 1
        self.caps = {
            "max_concurrency": 1000,
            "max_rate": 1000.0,
            "max_duration_s": 3600,
            "max_body_bytes": 65536,
        }
        self.probe = {"status": 200, "ttfb_ms": 201.0, "total_ms": 590.0, "error": None, "body_excerpt": ""}
        self.probe_state = "prepared"
        self.prepared: list[dict] = []
        self.started: list[str] = []
        self.cancelled: list[str] = []
        self.reports = 0
        self.state: str | None = None
        self.ended: str | None = None
        self.next_t = 0

    def interval(self, t: int) -> dict:
        n = self.per_second
        return {
            "t": t,
            "target": 2.0,
            "started": n,
            "completed": n,
            "ok": n - 1,
            "errors": {"connect": 0, "timeout": 0, "body": 0, "other": 0, "cancelled": 0},
            "status": {"200": n - 1, "503": 1},
            "bytes": 1000 * n,
            "in_flight": 2,
            "skipped": 0,
            "lag_ms": {"max": 0.2, "mean": 0.1},
            "cpu_pct": 50.0,
            "ttfb": hist(200_000, n - 1),
            "total": hist(600_000, n - 1),
            "gw_delay": hist(1500, n - 1),
        }

    def handle(self, request: httpx.Request) -> httpx.Response:
        if self.unreachable:
            raise httpx.ConnectError("connection refused", request=request)
        if request.headers.get("authorization") != f"Bearer {LOAD_TOKEN}":
            return httpx.Response(401, json={"error": {"code": "unauthorized", "message": "token"}})
        path = request.url.path
        if path == "/v1/info":
            busy = self.busy
            return httpx.Response(
                200,
                json={
                    "contract": self.contract,
                    "name": self.name,
                    "version": "0.1.0",
                    "target": "http://core:8080",
                    "caps": self.caps,
                    "cpus": 2,
                    "busy": busy,
                },
            )
        if path == "/v1/runs" and request.method == "POST":
            plan = json.loads(request.content)
            self.prepared.append(plan)
            self.state = self.probe_state
            return httpx.Response(
                201, json={"run_id": plan["run_id"], "state": self.probe_state, "probe": self.probe}
            )
        run_id = path.split("/")[3]
        if path.endswith("/start"):
            self.started.append(run_id)
            self.state = "running"
            return httpx.Response(202, json={"state": "running"})
        if path.endswith("/cancel"):
            self.cancelled.append(run_id)
            if self.state not in ("done", "failed"):
                self.state, self.ended = "cancelled", "cancelled"
            return httpx.Response(202, json={"state": "cancelling"})
        if path.endswith("/report"):
            self.reports += 1
            if self.answers is not None and self.reports > self.answers:
                raise httpx.ConnectError("gone", request=request)
            intervals = []
            if self.state == "running":
                if not self.endless and self.next_t >= self.seconds:
                    self.state, self.ended = "done", "duration"
                else:
                    intervals.append(self.interval(self.next_t))
                    self.next_t += 1
            return httpx.Response(
                200,
                json={
                    "run_id": run_id,
                    "state": self.state,
                    "ended": self.ended,
                    "error": "",
                    "elapsed_ms": self.next_t * 1000,
                    "in_flight": 2,
                    "calls": self.next_t * self.per_second,
                    "intervals": intervals,
                },
            )
        return httpx.Response(404, json={"error": {"code": "not_found", "message": path}})


def transport(core, agents: list[FakeAgent]) -> httpx.MockTransport:
    """The fake core and the fake agents behind one transport."""
    by_host = {a.host: a for a in agents}

    async def handle(request: httpx.Request) -> httpx.Response:
        agent = by_host.get(request.url.host)
        if agent is not None:
            return agent.handle(request)
        return await core.handle(request)

    return httpx.MockTransport(handle)
