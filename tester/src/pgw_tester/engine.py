"""The runner: one run at a time, suite by suite, with teardown after every suite."""

import asyncio
import contextlib
import logging
import secrets
import time
from dataclasses import dataclass, field
from typing import Any

import httpx
from pacod_id.client import ApiError, IdClient, IdConfig
from pacod_id.grant import DelegatedGrant, SignedOut

from . import fixtures
from .catalog import Catalog, Entry
from .config import Settings
from .model import Case, Step
from .redact import Redactor
from .results import CaseResult, Run, RunRequest, Starter, StepResult, SuiteResult, now
from .steps import Cancelled, Context, run_step
from .store import Store

log = logging.getLogger("pgw_tester")


class Busy(Exception):
    def __init__(self, run_id: str):
        super().__init__(f"run {run_id} is still going; wait for it or cancel it")
        self.run_id = run_id


class Invalid(Exception):
    pass


class NoGrant(Exception):
    """pacod-id did not answer the request for a delegated grant."""


@dataclass
class Active:
    run: Run
    cancel: asyncio.Event = field(default_factory=asyncio.Event)
    subscribers: set[asyncio.Queue] = field(default_factory=set)
    task: asyncio.Task | None = None


def new_run_id() -> str:
    return now().strftime("%Y%m%d-%H%M%S") + "-" + secrets.token_hex(3)


class Runner:
    def __init__(
        self,
        settings: Settings,
        catalog: Catalog,
        store: Store,
        transport: httpx.AsyncBaseTransport | None = None,
    ):
        self.settings = settings
        self.catalog = catalog
        self.store = store
        self.transport = transport
        self.active: Active | None = None
        self._lock = asyncio.Lock()
        self.identity = (
            IdClient(
                IdConfig(
                    issuers=(settings.id_issuer,) if settings.id_issuer else (),
                    internal_url=settings.id_internal_url,
                    client_id=settings.id_client_id,
                    client_secret=settings.id_client_secret,
                    product="gw",
                    audience="pacod-gw",
                ),
                httpx.AsyncClient(transport=transport, timeout=15, follow_redirects=False),
            )
            if settings.delegated
            else None
        )

    # --- control ---------------------------------------------------------------------------

    async def start(self, starter: Starter, token: str, req: RunRequest) -> Run:
        async with self._lock:
            if self.active is not None:
                raise Busy(self.active.run.id)
            plan = self._plan(req)
            # A run can outlast the admin's access token: it works with a grant of their session
            # instead, which ends when they sign out, and is revoked when the run ends.
            grant = await self._grant(token)
            try:
                run = Run(
                    id=new_run_id(),
                    mode=req.mode,
                    started_by=starter,
                    params=req.params,
                    stop_on_fail=req.stop_on_fail,
                    keep_resources=req.keep_resources,
                    suites=[
                        SuiteResult(
                            suite=e.suite.suite,
                            title=e.suite.title,
                            source=e.source,
                            cases=[CaseResult(id=c.id, title=c.title) for c in cases],
                        )
                        for e, cases in plan
                    ],
                )
                run.recount()
                self.store.save(run)
                self.store.prune()
                active = Active(run=run)
                self.active = active
                active.task = asyncio.create_task(self._execute(active, token, plan, grant))
            except BaseException:
                # No run took the grant over: it ends here, not with a run.
                self.active = None
                await _close(grant)
                raise
            return run

    async def _grant(self, token: str) -> DelegatedGrant | None:
        if self.identity is None:
            return None
        try:
            return await DelegatedGrant.start(self.identity, token)
        except SignedOut:
            raise
        except ApiError as e:
            raise NoGrant(f"pacod-id gave the run no grant of the session: {e}") from None

    def _plan(self, req: RunRequest) -> list[tuple[Entry, list[Case]]]:
        if req.mode == "mock" and not self.settings.mock_url:
            raise Invalid("mock mode needs a mock provider, and none is configured (TESTER_MOCK_URL)")
        if req.mode == "live" and not any(req.params.chosen().values()):
            raise Invalid("live mode needs at least one model to test with")
        plan = self.catalog.find(req.cases)
        if req.cases:
            known = {c.id for _, cases in plan for c in cases}
            unknown = sorted(set(req.cases) - known)
            if unknown:
                raise Invalid(f"unknown cases: {', '.join(unknown)}")
        if not plan:
            raise Invalid("there is nothing to run")
        return plan

    def cancel(self, run_id: str) -> bool:
        if self.active is None or self.active.run.id != run_id:
            return False
        self.active.cancel.set()
        return True

    def subscribe(self, run_id: str) -> asyncio.Queue | None:
        if self.active is None or self.active.run.id != run_id:
            return None
        queue: asyncio.Queue = asyncio.Queue(maxsize=256)
        self.active.subscribers.add(queue)
        return queue

    def unsubscribe(self, run_id: str, queue: asyncio.Queue) -> None:
        if self.active is not None and self.active.run.id == run_id:
            self.active.subscribers.discard(queue)

    def current(self, run_id: str) -> Run | None:
        """The live state of the active run, or the stored one."""
        if self.active is not None and self.active.run.id == run_id:
            return self.active.run
        return self.store.load(run_id)

    async def wait(self) -> None:
        """Waits for the active run to end (for tests and shutdown)."""
        if self.active and self.active.task:
            with contextlib.suppress(asyncio.CancelledError):
                await self.active.task

    # --- execution -------------------------------------------------------------------------

    def _publish(self, active: Active, event: str, **extra: Any) -> None:
        active.run.recount()
        self.store.save(active.run)
        payload = {"event": event, "run": active.run.summary(), **extra}
        for queue in list(active.subscribers):
            with contextlib.suppress(asyncio.QueueFull):
                queue.put_nowait(payload)

    async def _execute(
        self,
        active: Active,
        token: str,
        plan: list[tuple[Entry, list[Case]]],
        grant: DelegatedGrant | None = None,
    ) -> None:
        run = active.run
        run.status = "running"
        run.started_at = now()
        redact = Redactor([token])
        models: dict[str, dict[str, str]] | None = None
        try:
            self._publish(active, "started")
            async with httpx.AsyncClient(transport=self.transport, follow_redirects=False) as http:
                for (entry, cases), result in zip(plan, run.suites, strict=True):
                    ctx = Context(
                        settings=self.settings,
                        http=http,
                        redact=redact,
                        token=token,
                        cancel=active.cancel,
                        grant=grant,
                    )
                    if self.settings.mock_control_url and run.mode == "mock":
                        ctx.available.add("mock_control")
                    models = await self._suite(active, ctx, entry, cases, result, models)
            if active.cancel.is_set():
                run.status = "cancelled"
            else:
                run.recount()
                run.status = "failed" if run.counts.failed or run.counts.error else "passed"
        except SignedOut:
            # The admin signed out, or their session was ended: the run stops with it.
            run.status = "cancelled"
            run.error = "the session that started the run has ended"
        except Exception as e:  # the engine itself failed: keep the run readable
            log.exception("run %s failed", run.id)
            run.status = "error"
            run.error = redact(f"{type(e).__name__}: {e}")
        finally:
            try:
                run.finished_at = now()
                self._publish(active, "finished")
                for queue in list(active.subscribers):
                    with contextlib.suppress(asyncio.QueueFull):
                        queue.put_nowait(None)
            finally:
                self.active = None
                await _close(grant)

    async def _suite(
        self,
        active: Active,
        ctx: Context,
        entry: Entry,
        cases: list[Case],
        result: SuiteResult,
        models: dict[str, dict[str, str]] | None,
    ) -> dict[str, dict[str, str]] | None:
        run = active.run
        suite = entry.suite
        result.status = "running"
        ctx.variables.update({"run_id": run.id, "suffix": run.id[-6:], "mode": run.mode})
        self._publish(active, "suite", suite=suite.suite)
        setup_problem = ""
        signed_out: SignedOut | None = None
        try:
            if active.cancel.is_set():
                raise Cancelled()
            rec = fixtures.Recorder(ctx, result.setup)
            if suite.fixture == "scratch":
                await fixtures.scratch(rec, f"test-{run.id[-6:]}-{suite.suite}"[:63])
                if models is None:
                    if run.mode == "mock":
                        models = await fixtures.mock_models(rec)
                    else:
                        models = await fixtures.live_models(rec, run.params.chosen())
                for family, model in models.items():
                    await fixtures.grant(rec, family, model)
            for step in suite.setup:
                step_result = await run_step(ctx, step, ctx.variables)
                result.setup.append(step_result)
                if step_result.status in ("failed", "error"):
                    raise fixtures.FixtureFailed(f"{step_result.name}: {'; '.join(step_result.failures)}")
        except fixtures.FixtureFailed as e:
            setup_problem = f"the suite's setup failed: {e}"
        except Cancelled:
            setup_problem = "cancelled"
        except SignedOut as e:
            # The run ends with the session, after the teardown (which reports what it could
            # not delete).
            signed_out = e
            setup_problem = "cancelled"
            active.cancel.set()

        try:
            for case, case_result in zip(cases, result.cases, strict=True):
                skip = self._skip_reason(active, ctx, case, setup_problem)
                if skip:
                    case_result.status = (
                        "error" if skip == setup_problem and skip != "cancelled" else "skipped"
                    )
                    case_result.reason = skip
                    continue
                case_result.status = "running"
                self._publish(active, "case", suite=suite.suite, case=case.id)
                await self._case(ctx, case, case_result)
                self._publish(active, "case", suite=suite.suite, case=case.id)
        finally:
            await self._teardown(active, ctx, result)
            result.status = "done"
            self._publish(active, "suite", suite=suite.suite)
        if signed_out is not None:
            raise signed_out
        return models

    def _skip_reason(self, active: Active, ctx: Context, case: Case, setup_problem: str) -> str:
        run = active.run
        if active.cancel.is_set():
            return "cancelled"
        if setup_problem:
            return setup_problem
        if run.mode not in case.modes:
            return f"runs in {' and '.join(case.modes)} mode only"
        missing = [n for n in case.needs if n not in ctx.available]
        if missing:
            return "needs " + ", ".join(_NEEDS.get(n, n) for n in missing)
        if run.stop_on_fail and (run.counts.failed or run.counts.error):
            return "stopped after the first failure"
        return ""

    async def _case(self, ctx: Context, case: Case, result: CaseResult) -> None:
        started = time.monotonic()
        variables = dict(ctx.variables)
        result.status = "passed"
        try:
            for step in case.steps:
                step_result = await run_step(ctx, step, variables)
                result.steps.append(step_result)
                if step_result.status in ("failed", "error"):
                    result.status = step_result.status
                    result.reason = f"{step_result.name}: {'; '.join(step_result.failures)}"
                    break
        except Cancelled:
            result.status = "skipped"
            result.reason = "cancelled"
        result.duration_ms = round((time.monotonic() - started) * 1000, 1)

    async def _teardown(self, active: Active, ctx: Context, result: SuiteResult) -> None:
        """Deletes what the suite created, newest first. Runs after failures and cancels too."""
        if active.run.keep_resources:
            result.leftovers = list(reversed(ctx.ledger))
            return
        # Teardown ignores the cancel flag: it is what makes cancelling safe.
        ctx.cancel = asyncio.Event()
        paths = list(reversed(ctx.ledger))
        for i, path in enumerate(paths):
            try:
                step_result = await _delete(ctx, path)
            except SignedOut:
                # The session ended: nothing more can be deleted with it.
                result.leftovers.extend(paths[i:])
                return
            result.teardown.append(step_result)
            if step_result.status != "passed":
                result.leftovers.append(path)


async def _close(grant: DelegatedGrant | None) -> None:
    """Revokes a run's grant; a failure is logged (pacod-id lets it expire)."""
    if grant is None:
        return
    try:
        await grant.close()
    except Exception:
        log.warning("the run's grant was not revoked; it expires on its own", exc_info=True)


async def _delete(ctx: Context, path: str) -> StepResult:
    step = Step.model_validate(
        {
            "name": f"delete {path}",
            "mgmt": {"method": "DELETE", "path": path},
            "expect": {"status": [204, 404]},
        }
    )
    return await run_step(ctx, step, {})


_NEEDS: dict[str, str] = {
    "openai": "an OpenAI-dialect model",
    "anthropic": "an Anthropic model",
    "gemini": "a model on Gemini's own API",
    "mock_control": "a mock provider it can script",
}
