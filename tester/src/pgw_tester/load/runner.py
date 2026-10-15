"""The load runner: one load test at a time.

A run goes through these steps:
1. The tester checks the request and asks every chosen agent whether it is there, speaks this
   contract, is free, and allows its share of the load.
2. It sets up the scene (mock and live targets) and makes one call itself.
3. It prepares every agent at once. Each agent probes the target with one call.
4. It starts the agents together.
5. Once a second it collects each agent's seconds and adds them up into one point a second,
   which it keeps and streams to the console.
6. At the end, or on a cancel, it stops the agents, writes the summary and its warnings, and
   takes the scene down.

Agents run on a lease that every report renews: if the tester goes away, they stop on their own.
"""

import asyncio
import contextlib
import logging
import math
import time
from collections import Counter
from dataclasses import dataclass, field
from typing import Any

import httpx
from pacod_id.grant import DelegatedGrant, SignedOut

from ..config import Settings
from ..engine import new_run_id
from ..fixtures import FixtureFailed, Recorder
from ..guard import Exclusive
from ..redact import Redactor
from ..results import Starter, now
from ..session import close_grant, identity_client, take_grant
from ..steps import Cancelled, Context
from . import requests, scene
from .agents import AgentClient, AgentError, AgentRef
from .contract import CONTRACT, FINISHED_STATES, Errors, Info, Interval, Prepare, Profile, RequestSpec
from .histogram import Histogram
from .model import (
    LOAD_FINISHED,
    AgentPart,
    GatewaySample,
    LoadRequest,
    LoadRun,
    LoadStore,
    Pct,
    Point,
    Share,
    Summary,
)
from .split import shares

log = logging.getLogger("pgw_tester")

#: How often the agents are asked for their seconds.
POLL_EVERY = 1.0
#: How often a running load test is written to disk.
SAVE_EVERY = 5.0
#: How often the gateway's own figures are read.
SAMPLE_EVERY = 2.0
#: A second is shown without an agent that has not reported it after this long.
STALE_AFTER = 3.0
#: Failed reports in a row after which an agent counts as lost (its lease stops it).
LOST_AFTER = 10
#: The agents' lease, in seconds.
LEASE_S = 30
#: How long running calls may take to finish after the last stage.
DRAIN_S = 30
#: Connections an agent opens before the start, at most.
WARMUP_MAX = 200
CPU_WARN = 85.0
LAG_WARN_MS = 50.0


class Invalid(Exception):
    """The request cannot run as asked (422)."""


class AgentsBusy(Exception):
    """A chosen agent is running something (409)."""


class AgentsUnavailable(Exception):
    """A chosen agent does not answer (502)."""


class PrepareFailed(Exception):
    pass


@dataclass
class Leg:
    """One agent in the run."""

    client: AgentClient
    part: AgentPart
    cpus: int = 1
    last_t: int | None = None
    pending: dict[int, Interval] = field(default_factory=dict)
    failures: int = 0
    finished: bool = False
    lost: bool = False
    elapsed_ms: int = 0


@dataclass
class Totals:
    started: int = 0
    completed: int = 0
    ok: int = 0
    skipped: int = 0
    bytes: int = 0
    status: Counter = field(default_factory=Counter)
    errors: Errors = field(default_factory=Errors)
    ttfb: Histogram = field(default_factory=Histogram)
    total: Histogram = field(default_factory=Histogram)
    gw_delay: Histogram = field(default_factory=Histogram)
    lag_max: float = 0

    def add(self, iv: Interval) -> tuple[Histogram, Histogram, Histogram]:
        self.started += iv.started
        self.completed += iv.completed
        self.ok += iv.ok
        self.skipped += iv.skipped
        self.bytes += iv.bytes
        self.status.update({str(k): v for k, v in iv.status.items()})
        for name in ("connect", "timeout", "body", "other", "cancelled"):
            setattr(self.errors, name, getattr(self.errors, name) + getattr(iv.errors, name))
        self.lag_max = max(self.lag_max, iv.lag_ms.max)
        hs = (
            Histogram.of(iv.ttfb.model_dump()),
            Histogram.of(iv.total.model_dump()),
            Histogram.of(iv.gw_delay.model_dump()),
        )
        self.ttfb.merge(hs[0])
        self.total.merge(hs[1])
        self.gw_delay.merge(hs[2])
        return hs


@dataclass
class ActiveLoad:
    run: LoadRun
    req: LoadRequest
    token: str
    grant: DelegatedGrant | None
    legs: list[Leg]
    cancel: asyncio.Event = field(default_factory=asyncio.Event)
    subscribers: set[asyncio.Queue] = field(default_factory=set)
    task: asyncio.Task | None = None
    totals: Totals = field(default_factory=Totals)
    next_t: int = 0
    first_seen: dict[int, float] = field(default_factory=dict)
    gateway: GatewaySample | None = None
    t0: float = 0.0


def pct(h: Histogram) -> Pct:
    ms = 1000.0
    return Pct(
        p50=round(h.percentile(0.5) / ms, 3),
        p95=round(h.percentile(0.95) / ms, 3),
        p99=round(h.percentile(0.99) / ms, 3),
        max=round(h.max / ms, 3),
    )


class LoadRunner:
    def __init__(
        self,
        settings: Settings,
        store: LoadStore,
        exclusive: Exclusive,
        transport: httpx.AsyncBaseTransport | None = None,
    ):
        self.settings = settings
        self.store = store
        self.exclusive = exclusive
        self.transport = transport
        self.identity = identity_client(settings, transport)
        self.agents: dict[str, AgentRef] = {a.name: a for a in settings.load_agents}
        self.http = httpx.AsyncClient(transport=transport, follow_redirects=False)
        self.active: ActiveLoad | None = None

    async def aclose(self) -> None:
        await self.http.aclose()

    def client(self, name: str) -> AgentClient:
        return AgentClient(self.agents[name], self.settings.load_token, self.http)

    # --- what the console shows before a run ------------------------------------------------

    async def agents_view(self) -> list[dict[str, Any]]:
        """Every configured agent, asked whether it is there."""

        async def one(ref: AgentRef) -> dict[str, Any]:
            out: dict[str, Any] = {"name": ref.name, "url": ref.url}
            try:
                info = await self.client(ref.name).info()
            except AgentError as e:
                return {**out, "reachable": False, "error": str(e)}
            return {**out, "reachable": True, "error": "", **info.model_dump(mode="json")}

        return list(await asyncio.gather(*(one(a) for a in self.agents.values())))

    # --- control ---------------------------------------------------------------------------

    @staticmethod
    def overhead(req: LoadRequest) -> int:
        """Calls a run makes besides the load: one probe per agent, and the tester's own call
        before the load on this gateway's scene."""
        return len(req.agents) + (1 if req.target in ("mock", "live") else 0)

    def check(self, req: LoadRequest) -> None:
        s = self.settings
        if not self.agents or not s.load_token:
            raise Invalid("no load agents are configured (TESTER_LOAD_AGENTS and TESTER_LOAD_TOKEN)")
        unknown = [a for a in req.agents if a not in self.agents]
        if unknown:
            raise Invalid(f"unknown agents: {', '.join(unknown)}")
        if req.target == "mock" and not s.load_mock_url:
            raise Invalid(
                "a mock target needs a mock provider, and none is configured (TESTER_LOAD_MOCK_URL)"
            )
        if req.load.duration_s() > s.load_max_duration:
            raise Invalid(
                f"a load test lasts at most {s.load_max_duration} s here (TESTER_LOAD_MAX_DURATION)"
            )
        if req.load.peak() <= 0:
            raise Invalid("no stage has a target above 0")
        if req.target in ("live", "own", "external"):
            if req.load.max_calls is None:
                raise Invalid("a load test that may reach a paid provider needs a number of calls at most")
            if not req.acknowledge_cost:
                raise Invalid("confirm that the calls may cost money")
        if (
            req.target == "live"
            and req.load.max_calls is not None
            and req.load.max_calls > s.load_live_max_calls
        ):
            raise Invalid(
                f"a live load test makes at most {s.load_live_max_calls} calls here "
                "(TESTER_LOAD_LIVE_MAX_CALLS)"
            )
        if req.load.max_calls is not None and req.load.max_calls - self.overhead(req) < len(req.agents):
            raise Invalid(
                f"{req.load.max_calls} calls leave too few for {len(req.agents)} agents after the "
                f"{self.overhead(req)} calls that check the target first"
            )
        if req.load.mode == "rate" and (req.load.max_in_flight or 0) < len(req.agents):
            raise Invalid("max_in_flight is split across the agents: give each at least 1")

    def _budget(self, req: LoadRequest) -> int | None:
        return None if req.load.max_calls is None else req.load.max_calls - self.overhead(req)

    @staticmethod
    def _fits(name: str, info: Info, share: Share, duration: int) -> str:
        caps = info.caps
        peak = max(s.target for s in share.stages)
        if duration > caps.max_duration_s:
            return f"agent {name} runs at most {caps.max_duration_s} s"
        if share.max_in_flight is None and peak > caps.max_concurrency:
            return f"agent {name} makes at most {caps.max_concurrency} calls at a time, its share is {peak:g}"
        if share.max_in_flight is not None:
            if peak > caps.max_rate:
                return f"agent {name} makes at most {caps.max_rate:g} calls a second, its share is {peak:g}"
            if share.max_in_flight > caps.max_concurrency:
                return f"agent {name} holds at most {caps.max_concurrency} calls at a time"
        return ""

    async def start(self, starter: Starter, token: str, req: LoadRequest) -> LoadRun:
        run_id = new_run_id()
        # Taken before the first await: a functional run starting meanwhile is refused.
        self.exclusive.take("load", run_id)
        grant: DelegatedGrant | None = None
        try:
            self.check(req)
            parts = shares(req.load, len(req.agents), self._budget(req))
            clients = [self.client(n) for n in req.agents]
            infos = await asyncio.gather(*(c.info() for c in clients), return_exceptions=True)
            legs: list[Leg] = []
            for client, info, share in zip(clients, infos, parts, strict=True):
                if isinstance(info, AgentError):
                    raise AgentsUnavailable(str(info))
                if isinstance(info, BaseException):
                    raise info
                if info.contract != CONTRACT:
                    raise Invalid(
                        f"agent {client.name} speaks contract {info.contract}, this tester {CONTRACT}"
                    )
                if info.busy is not None:
                    raise AgentsBusy(
                        f"agent {client.name} is busy with {info.busy.run_id} ({info.busy.state})"
                    )
                problem = self._fits(client.name, info, share, req.load.duration_s())
                if problem:
                    raise Invalid(problem)
                part = AgentPart(
                    name=client.name,
                    url=client.ref.url,
                    version=info.version,
                    target=info.target,
                    cpus=info.cpus,
                    share=share,
                )
                legs.append(Leg(client=client, part=part, cpus=max(1, info.cpus)))
            # The scene needs the admin's session for the whole run; an external target needs
            # none of this gateway's management API.
            if req.target != "external":
                grant = await take_grant(self.identity, token)
        except BaseException:
            self.exclusive.release("load", run_id)
            raise
        try:
            run = LoadRun(
                id=run_id,
                label=req.label,
                target=req.target,
                started_by=starter,
                request=req.public(),
                base_url=req.external.base_url if req.target == "external" and req.external else "",
                agents=[leg.part for leg in legs],
            )
            self.store.save(run)
            self.store.prune()
            active = ActiveLoad(run=run, req=req, token=token, grant=grant, legs=legs)
            self.active = active
            active.task = asyncio.create_task(self._execute(active))
        except BaseException:
            self.active = None
            self.exclusive.release("load", run_id)
            await close_grant(grant)
            raise
        return run

    def cancel(self, run_id: str) -> bool:
        if self.active is None or self.active.run.id != run_id:
            return False
        self.active.cancel.set()
        return True

    def current(self, run_id: str) -> LoadRun | None:
        if self.active is not None and self.active.run.id == run_id:
            return self.active.run
        return self.store.load(run_id)

    def subscribe(self, run_id: str) -> asyncio.Queue | None:
        if self.active is None or self.active.run.id != run_id:
            return None
        queue: asyncio.Queue = asyncio.Queue(maxsize=512)
        self.active.subscribers.add(queue)
        return queue

    def unsubscribe(self, run_id: str, queue: asyncio.Queue) -> None:
        if self.active is not None and self.active.run.id == run_id:
            self.active.subscribers.discard(queue)

    async def wait(self) -> None:
        if self.active and self.active.task:
            with contextlib.suppress(asyncio.CancelledError):
                await self.active.task

    # --- execution -------------------------------------------------------------------------

    def _publish(self, active: ActiveLoad, item: dict[str, Any] | None) -> None:
        for queue in list(active.subscribers):
            with contextlib.suppress(asyncio.QueueFull):
                queue.put_nowait(item)

    def _status(self, active: ActiveLoad, status: str) -> None:
        active.run.status = status  # type: ignore[assignment]
        self.store.save(active.run)
        self._publish(active, {"event": "status", "status": status, "run": active.run.view()})

    async def _execute(self, active: ActiveLoad) -> None:
        run, req = active.run, active.req
        run.started_at = now()
        redact = Redactor([active.token])
        for keyed in (req.own, req.external):
            if keyed is not None:
                redact.add(keyed.secret)
        status = "finished"
        prepared = False
        async with httpx.AsyncClient(transport=self.transport, follow_redirects=False) as http:
            ctx = Context(
                settings=self.settings,
                http=http,
                redact=redact,
                token=active.token,
                cancel=active.cancel,
                grant=active.grant,
            )
            try:
                self._status(active, "preparing")
                key, base_url, spec = await self._scene(active, ctx)
                if active.cancel.is_set():
                    raise Cancelled()
                prepared = True
                await self._prepare(active, key, base_url, spec, redact)
                if active.cancel.is_set():
                    raise Cancelled()
                await self._start(active)
                self._status(active, "running")
                await self._follow(active, ctx)
                if active.cancel.is_set():
                    status = "cancelled"
            except Cancelled:
                status = "cancelled"
            except SignedOut:
                status = "cancelled"
                run.error = "the session that started the load test has ended"
            except (FixtureFailed, AgentError, PrepareFailed) as e:
                status = "error"
                run.error = redact(str(e))
            except Exception as e:
                log.exception("load test %s failed", run.id)
                status = "error"
                run.error = redact(f"{type(e).__name__}: {e}")
            finally:
                try:
                    if prepared:
                        await self._stop_agents(active)
                    self._summarise(active)
                    await self._clean(active, ctx)
                    run.status = status  # type: ignore[assignment]
                    run.finished_at = now()
                    self.store.save(run)
                    self._publish(active, {"event": "status", "status": status, "run": run.view()})
                    self._publish(active, None)
                finally:
                    self.active = None
                    self.exclusive.release("load", run.id)
                    await close_grant(active.grant)

    async def _scene(self, active: ActiveLoad, ctx: Context) -> tuple[str, str | None, RequestSpec]:
        """Sets up what the run calls; returns the key, the base URL (external only) and the
        request."""
        run, req = active.run, active.req
        if req.target == "own" and req.own is not None:
            spec = requests.build(req, req.own.model, req.own.provider, req.own.upstream)
            return req.own.secret, None, spec
        if req.target == "external" and req.external is not None:
            spec = requests.build(req, req.external.model, req.external.provider, req.external.upstream)
            return req.external.secret, req.external.base_url, spec
        rec = Recorder(ctx, run.scene.setup)
        try:
            if req.target == "mock" and req.mock is not None:
                family = "openai" if req.interface == "embeddings" else req.mock.family
                run.scene.profile = req.mock.spec()
                model = await scene.ensure_load_mock(
                    rec, family, run.scene.profile, req.interface == "embeddings"
                )
            else:
                assert req.live is not None
                model = await scene.live_model(rec, req.live.model_id)
            if req.load.mode == "concurrency":
                peak_in_flight = math.ceil(req.load.peak())
            else:
                peak_in_flight = req.load.max_in_flight or 1
            limit = req.load.max_calls if req.target == "live" else None
            await scene.scratch_scene(rec, f"load-{run.id[-6:]}", req, model, peak_in_flight, limit)
        finally:
            run.scene.ledger = list(ctx.ledger)
            self.store.save(run)
        v = ctx.variables
        run.scene.org, run.scene.ws, run.scene.key_id = v.get("org", ""), v.get("ws", ""), v.get("key_id", "")
        run.scene.model, run.scene.model_id = model["name"], model["id"]
        run.scene.provider, run.scene.upstream = model["provider"], model["upstream"]
        spec = requests.build(req, model["name"], model["provider"], model["upstream"])
        await scene.sanity_call(ctx, run.scene.setup, spec.path, spec.headers, spec.body)
        self.store.save(run)
        return v["key"], None, spec

    async def _prepare(
        self, active: ActiveLoad, key: str, base_url: str | None, spec: RequestSpec, redact: Redactor
    ) -> None:
        """Prepares every agent at once: one slow prepare must not let another's lease run out."""
        req, run = active.req, active.run
        plans = []
        for i, leg in enumerate(active.legs):
            share = leg.part.share
            assert share is not None
            peak = max(s.target for s in share.stages)
            warm = share.max_in_flight if share.max_in_flight is not None else math.ceil(peak)
            plans.append(
                Prepare(
                    run_id=run.id,
                    part=i,
                    parts=len(active.legs),
                    key=key,
                    auth_header=requests.auth_header(req.interface),
                    base_url=base_url,
                    request=spec,
                    profile=Profile(
                        mode=req.load.mode,
                        stages=share.stages,
                        max_in_flight=share.max_in_flight,
                        timeout_s=req.load.timeout_s,
                        max_calls=share.max_calls,
                        drain_s=DRAIN_S,
                        http=req.http,
                        warmup_connections=min(warm, WARMUP_MAX),
                    ),
                    lease_s=LEASE_S,
                )
            )
        timeout = req.load.timeout_s + 30
        results = await asyncio.gather(
            *(leg.client.prepare(plan, timeout) for leg, plan in zip(active.legs, plans, strict=True)),
            return_exceptions=True,
        )
        problems: list[str] = []
        for leg, result in zip(active.legs, results, strict=True):
            if isinstance(result, BaseException):
                leg.part.state = "failed"
                leg.part.error = redact(str(result))
                problems.append(leg.part.error)
                continue
            probe = result.probe
            probe.body_excerpt = redact(probe.body_excerpt)
            leg.part.probe = probe
            leg.part.state = result.state
            if result.state != "prepared" or probe.error or not (200 <= (probe.status or 0) < 300):
                said = probe.error or f"answered {probe.status}"
                excerpt = f": {probe.body_excerpt[:200]}" if probe.body_excerpt else ""
                problems.append(f"agent {leg.part.name}: the call before the load {said}{excerpt}")
        self.store.save(run)
        if problems:
            raise PrepareFailed("; ".join(problems))

    async def _start(self, active: ActiveLoad) -> None:
        results = await asyncio.gather(
            *(leg.client.start(active.run.id) for leg in active.legs), return_exceptions=True
        )
        active.t0 = time.monotonic()
        for leg, result in zip(active.legs, results, strict=True):
            if isinstance(result, BaseException):
                raise result
            leg.part.state = "running"

    async def _stop_agents(self, active: ActiveLoad) -> None:
        """Cancels every agent that may still be going (prepared, running, or not heard from)."""
        going = [leg for leg in active.legs if not leg.finished and leg.part.state not in FINISHED_STATES]
        await asyncio.gather(*(leg.client.cancel(active.run.id) for leg in going), return_exceptions=True)

    async def _follow(self, active: ActiveLoad, ctx: Context) -> None:
        """Collects the agents' seconds until all have ended."""
        req, run = active.req, active.run
        sampler = asyncio.create_task(self._sample(active, ctx)) if req.target != "external" else None
        cancel_sent = False
        last_save = time.monotonic()
        give_up = time.monotonic() + req.load.duration_s() + DRAIN_S + 60
        cancel_deadline = 0.0
        try:
            while True:
                if active.cancel.is_set() and not cancel_sent:
                    cancel_sent = True
                    cancel_deadline = time.monotonic() + 15
                    self._status(active, "draining")
                    await self._stop_agents(active)
                await self._poll(active)
                if run.status == "running" and any(leg.part.state == "draining" for leg in active.legs):
                    self._status(active, "draining")
                done = all(leg.finished or leg.lost for leg in active.legs)
                self._emit(active, flush=done)
                if done:
                    break
                now_m = time.monotonic()
                if cancel_sent and now_m > cancel_deadline:
                    run.warnings.append("the agents did not confirm the cancel in time")
                    break
                if now_m > give_up:
                    run.warnings.append("the agents did not end their run in time; it was cancelled")
                    await self._stop_agents(active)
                    break
                if now_m - last_save >= SAVE_EVERY:
                    self.store.save(run)
                    last_save = now_m
                if cancel_sent:
                    await asyncio.sleep(POLL_EVERY)
                else:
                    with contextlib.suppress(TimeoutError):
                        await asyncio.wait_for(active.cancel.wait(), POLL_EVERY)
        finally:
            if sampler is not None:
                sampler.cancel()
                with contextlib.suppress(asyncio.CancelledError):
                    await sampler
        if all(leg.lost for leg in active.legs):
            raise AgentError("all", "no agent answered any more; their leases stop the load")

    async def _poll(self, active: ActiveLoad) -> None:
        legs = [leg for leg in active.legs if not (leg.finished or leg.lost)]
        reports = await asyncio.gather(
            *(leg.client.report(active.run.id, leg.last_t) for leg in legs), return_exceptions=True
        )
        for leg, rep in zip(legs, reports, strict=True):
            if isinstance(rep, BaseException):
                leg.failures += 1
                if leg.failures >= LOST_AFTER:
                    leg.lost = True
                    leg.part.state = "lost"
                    leg.part.error = str(rep)
                    active.run.warnings.append(
                        f"agent {leg.part.name} stopped answering; its lease stopped its part of the load"
                    )
                continue
            leg.failures = 0
            leg.part.state = rep.state
            leg.part.ended = rep.ended
            leg.part.error = rep.error
            leg.part.calls = rep.calls
            leg.elapsed_ms = rep.elapsed_ms
            for iv in rep.intervals:
                leg.last_t = iv.t if leg.last_t is None else max(leg.last_t, iv.t)
                leg.part.last_t = leg.last_t
                if iv.t < active.next_t:
                    # Too late for its second's point: it still counts in the totals.
                    active.totals.add(iv)
                    continue
                leg.pending[iv.t] = iv
                active.first_seen.setdefault(iv.t, time.monotonic())
            if rep.state in FINISHED_STATES:
                leg.finished = True
                if rep.ended == "lease_expired":
                    active.run.warnings.append(f"agent {leg.part.name} stopped when its lease ran out")

    def _emit(self, active: ActiveLoad, flush: bool) -> None:
        """Turns the seconds every agent has reported into points."""
        legs = active.legs
        while True:
            t = active.next_t
            have = [leg for leg in legs if t in leg.pending]
            if not have:
                later = any(any(k > t for k in leg.pending) for leg in legs)
                if flush and later:
                    active.next_t += 1
                    continue
                return
            waiting = [
                leg
                for leg in legs
                if not (leg.finished or leg.lost) and (leg.last_t is None or leg.last_t < t)
            ]
            if waiting and not flush and time.monotonic() - active.first_seen.get(t, 0) < STALE_AFTER:
                return
            point = self._point(active, t, [(leg, leg.pending.pop(t)) for leg in have])
            active.run.points.append(point)
            self._publish(
                active,
                {
                    "event": "point",
                    "point": point.model_dump(mode="json"),
                    "agents": [
                        {"name": leg.part.name, "state": leg.part.state, "calls": leg.part.calls}
                        for leg in legs
                    ],
                    "status": active.run.status,
                },
            )
            active.next_t += 1

    def _point(self, active: ActiveLoad, t: int, ivs: list[tuple[Leg, Interval]]) -> Point:
        ttfb, total, gw = Histogram(), Histogram(), Histogram()
        status: Counter = Counter()
        p = Point(t=t, agents=len(ivs))
        cpu: float | None = None
        for leg, iv in ivs:
            h = active.totals.add(iv)
            ttfb.merge(h[0])
            total.merge(h[1])
            gw.merge(h[2])
            p.target += iv.target
            p.started += iv.started
            p.completed += iv.completed
            p.ok += iv.ok
            p.cancelled += iv.errors.cancelled
            p.bytes += iv.bytes
            p.in_flight += iv.in_flight
            p.skipped += iv.skipped
            p.lag_max_ms = max(p.lag_max_ms, iv.lag_ms.max)
            status.update({str(k): v for k, v in iv.status.items()})
            if iv.cpu_pct is not None:
                share = iv.cpu_pct / leg.cpus
                cpu = share if cpu is None else max(cpu, share)
                leg.part.cpu_max = round(max(leg.part.cpu_max, share), 1)
            leg.part.lag_max_ms = round(max(leg.part.lag_max_ms, iv.lag_ms.max), 2)
        p.failed = p.completed - p.ok
        p.status = dict(status)
        p.cpu_max = None if cpu is None else round(cpu, 1)
        p.ttfb = pct(ttfb) if ttfb.n else None
        p.total = pct(total) if total.n else None
        p.gw_delay = pct(gw) if gw.n else None
        if active.gateway is not None:
            p.gw_in_flight = active.gateway.in_flight
        return p

    async def _sample(self, active: ActiveLoad, ctx: Context) -> None:
        """The gateway's own figures, every few seconds: calls in flight across its instances,
        refusals, and whether usage events pile up."""
        url = f"{self.settings.admin_url}/api/v1/monitoring/live"
        while True:
            try:
                token = await ctx.session_token()
                r = await ctx.http.get(url, headers={"authorization": f"Bearer {token}"}, timeout=5)
                if r.status_code == 200:
                    # The traffic figures sit at the top level (the core flattens them).
                    live = r.json()
                    sample = GatewaySample(
                        t=round(time.monotonic() - active.t0, 1),
                        in_flight=int(live.get("in_flight") or 0),
                        requests_per_second=float(live.get("requests_per_second") or 0),
                        rejected_per_second=float(live.get("rejected_per_second") or 0),
                        spool_bytes=int(live.get("spool_bytes") or 0),
                        events_dropped=int(live.get("events_dropped") or 0),
                    )
                    active.gateway = sample
                    active.run.gateway.append(sample)
            except SignedOut:
                return
            except (httpx.HTTPError, ValueError, TypeError):
                pass
            await asyncio.sleep(SAMPLE_EVERY)

    def _summarise(self, active: ActiveLoad) -> None:
        run, tot = active.run, active.totals
        # Seconds still pending (the run ended before every agent reported them) count too.
        self._emit(active, flush=True)
        duration = max((leg.elapsed_ms for leg in active.legs), default=0) / 1000
        status = dict(tot.status)
        run.summary = Summary(
            duration_s=round(duration, 1),
            started=tot.started,
            calls=tot.completed,
            ok=tot.ok,
            failed=tot.completed - tot.ok,
            cancelled=tot.errors.cancelled,
            skipped=tot.skipped,
            status=status,
            errors=tot.errors,
            bytes=tot.bytes,
            rps_mean=round(tot.completed / duration, 1) if duration > 0 else 0,
            rps_peak=max((p.completed for p in run.points), default=0),
            ttfb=pct(tot.ttfb),
            total=pct(tot.total),
            gw_delay=pct(tot.gw_delay) if tot.gw_delay.n else None,
        )
        warnings = run.warnings
        for leg in active.legs:
            if leg.part.cpu_max >= CPU_WARN:
                warnings.append(
                    f"agent {leg.part.name} used up to {leg.part.cpu_max:.0f}% of its {leg.cpus} CPUs: "
                    "its figures may describe the agent rather than the gateway"
                )
        if tot.lag_max > LAG_WARN_MS:
            warnings.append(
                f"the agents started calls up to {tot.lag_max:.0f} ms late: they are short of CPU, "
                "or the rate is more than they can drive"
            )
        if tot.skipped:
            warnings.append(
                f"{tot.skipped} calls were not started because max_in_flight calls were running: "
                "the target did not keep up with the rate"
            )
        refused = {
            "503": "503 (the gateway was overloaded, or a provider unavailable)",
            "429": "429 (a rate limit or a concurrency cap)",
            "402": "402 (a quota, such as the run's own call limit)",
        }
        for code, why in refused.items():
            if status.get(code):
                warnings.append(f"{status[code]} calls were refused with {why}")
        if tot.errors.timeout:
            warnings.append(f"{tot.errors.timeout} calls took longer than the timeout")
        if active.run.gateway and active.run.gateway[-1].events_dropped:
            warnings.append("the gateway dropped usage events during the run: analytics did not keep up")

    async def _clean(self, active: ActiveLoad, ctx: Context) -> None:
        run = active.run
        if not run.scene.ledger and not ctx.ledger:
            return
        ledger = list(ctx.ledger) or run.scene.ledger
        run.scene.ledger = ledger
        if active.req.keep_resources:
            run.scene.leftovers = list(reversed(ledger))
            return
        self._status(active, "cleaning")
        # The teardown ignores the cancel: it is what makes cancelling safe.
        ctx.cancel = asyncio.Event()
        try:
            run.scene.leftovers = await scene.teardown(ctx, run.scene.teardown, ledger)
        except SignedOut:
            done = {r.name.removeprefix("delete ") for r in run.scene.teardown if r.status == "passed"}
            run.scene.leftovers = [p for p in ledger if p not in done]


__all__ = ["LOAD_FINISHED", "AgentsBusy", "AgentsUnavailable", "Invalid", "LoadRunner"]
