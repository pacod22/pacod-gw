"""The tester's HTTP API. Its only client is the server side of the console."""

import asyncio
import contextlib
import json
import logging
from collections.abc import AsyncIterator, Callable
from typing import Annotated

import httpx
from fastapi import Depends, FastAPI, HTTPException, Request, Response
from fastapi.exceptions import RequestValidationError
from fastapi.responses import JSONResponse, StreamingResponse
from pacod_id.grant import SignedOut

from .auth import Admin, SessionCheck
from .catalog import Catalog, CatalogError
from .config import Settings
from .engine import Busy, Invalid, NoGrant, Runner
from .guard import Exclusive
from .load.model import LOAD_FINISHED, LoadRequest, LoadStore
from .load.runner import AgentsBusy, AgentsUnavailable, LoadRunner
from .load.runner import Invalid as LoadInvalid
from .messages import Busy as MessagesBusy
from .messages import Message, MessageRequest, Messenger, message_summary
from .results import FINISHED, RunRequest, Starter
from .store import JsonStore, Store

KEEPALIVE_SECONDS = 15.0


def create_app(
    settings: Settings | None = None, transport: httpx.AsyncBaseTransport | None = None
) -> FastAPI:
    settings = settings or Settings.from_env()
    store = Store(settings.data_dir / "runs", settings.keep_runs)
    store.recover()
    catalog = Catalog(settings.data_dir / "cases")
    # A functional run and a load test exclude each other.
    exclusive = Exclusive()
    runner = Runner(settings, catalog, store, transport, exclusive)
    load_store = LoadStore(settings.data_dir / "load", settings.keep_load_runs)
    load_store.recover()
    loads = LoadRunner(settings, load_store, exclusive, transport)
    messages = JsonStore(settings.data_dir / "messages", Message, message_summary, settings.keep_runs)
    messenger = Messenger(settings, messages, transport)
    check = SessionCheck(settings.admin_url, transport)
    AdminDep = Annotated[Admin, Depends(check)]

    @contextlib.asynccontextmanager
    async def lifespan(_: FastAPI) -> AsyncIterator[None]:
        yield
        if runner.active:
            runner.active.cancel.set()
            await runner.wait()
        if loads.active:
            loads.active.cancel.set()
            await loads.wait()
        await loads.aclose()

    app = FastAPI(title="pacod-gw tester", version="0.1.0", lifespan=lifespan, docs_url=None, redoc_url=None)
    app.state.runner = runner
    app.state.loads = loads

    @app.exception_handler(RequestValidationError)
    async def invalid(_: Request, e: RequestValidationError) -> JSONResponse:
        # One sentence per problem, without echoing the values: a body may hold a key.
        errors = [f"{'.'.join(str(p) for p in err['loc'][1:]) or 'body'}: {err['msg']}" for err in e.errors()]
        return JSONResponse({"detail": "The request is not valid.", "errors": errors}, status_code=422)

    @app.get("/healthz")
    async def healthz() -> dict:
        return {"status": "ok"}

    @app.get("/catalog")
    async def get_catalog(_: AdminDep) -> dict:
        return {
            "mock_available": bool(settings.mock_url),
            "mock_control_available": bool(settings.mock_control_url),
            "suites": [
                {
                    "suite": e.suite.suite,
                    "title": e.suite.title,
                    "description": e.suite.description,
                    "source": e.source,
                    "fixture": e.suite.fixture,
                    "cases": [
                        {
                            "id": c.id,
                            "title": c.title,
                            "description": c.description,
                            "modes": c.modes,
                            "needs": c.needs,
                        }
                        for c in e.suite.cases
                    ],
                }
                for e in catalog.entries()
            ],
        }

    @app.get("/cases/{suite}")
    async def get_suite_file(suite: str, _: AdminDep) -> Response:
        for e in catalog.entries():
            if e.suite.suite == suite:
                return Response(e.file.read_text(), media_type="application/yaml")
        raise HTTPException(404, "There is no such suite.")

    @app.post("/cases", status_code=201)
    async def upload(request: Request, _: AdminDep) -> dict:
        text = (await request.body()).decode("utf-8", errors="replace")
        try:
            suite = catalog.upload(text)
        except CatalogError as e:
            return JSONResponse({"detail": "The suite is not valid.", "errors": e.errors}, status_code=422)
        return {"suite": suite.suite, "cases": len(suite.cases)}

    @app.delete("/cases/{suite}", status_code=204)
    async def delete_suite(suite: str, _: AdminDep) -> Response:
        if not catalog.delete(suite):
            raise HTTPException(404, "There is no uploaded suite of that name; built-in suites stay.")
        return Response(status_code=204)

    @app.post("/runs", status_code=202)
    async def start(body: RunRequest, admin: AdminDep) -> dict:
        try:
            run = await runner.start(Starter(id=admin.id, display=admin.display), admin.token, body)
        except Busy as e:
            raise HTTPException(409, str(e)) from None
        except Invalid as e:
            raise HTTPException(422, str(e)) from None
        except SignedOut:
            raise HTTPException(401, "The session has ended; sign in again.") from None
        except NoGrant as e:
            raise HTTPException(502, str(e)) from None
        return run.summary()

    @app.get("/runs")
    async def runs(_: AdminDep) -> dict:
        items = store.summaries()
        if runner.active:
            live = runner.active.run.summary()
            items = [live if r["id"] == live["id"] else r for r in items]
        return {"items": items, "active": runner.active.run.id if runner.active else None}

    @app.get("/runs/{run_id}")
    async def run(run_id: str, _: AdminDep) -> dict:
        found = runner.current(run_id)
        if found is None:
            raise HTTPException(404, "There is no such run.")
        return found.model_dump(mode="json")

    @app.post("/runs/{run_id}/cancel", status_code=202)
    async def cancel(run_id: str, _: AdminDep) -> dict:
        if not runner.cancel(run_id):
            raise HTTPException(409, "The run is not going.")
        return {"cancelling": run_id}

    @app.post("/messages", status_code=201)
    async def send_message(body: MessageRequest, admin: AdminDep) -> dict:
        try:
            message = await messenger.send(admin.id, admin.display, admin.token, body)
        except MessagesBusy as e:
            raise HTTPException(429, str(e)) from None
        return message.model_dump(mode="json")

    @app.get("/messages")
    async def list_messages(_: AdminDep) -> dict:
        return {"items": messages.summaries()}

    @app.get("/messages/{message_id}")
    async def get_message(message_id: str, _: AdminDep) -> dict:
        found = messages.load(message_id)
        if found is None:
            raise HTTPException(404, "There is no such message.")
        return found.model_dump(mode="json")

    @app.get("/runs/{run_id}/events")
    async def events(run_id: str, request: Request, _: AdminDep) -> StreamingResponse:
        found = runner.current(run_id)
        if found is None:
            raise HTTPException(404, "There is no such run.")
        queue = runner.subscribe(run_id)
        return _stream(
            request,
            queue,
            snapshot=lambda: {"event": "snapshot", "run": found.summary()},
            end=lambda: {"event": "end", "run": found.summary()},
            finished=found.status in FINISHED,
            close=lambda q: runner.unsubscribe(run_id, q),
            event="progress",
        )

    # --- load tests -------------------------------------------------------------------------

    @app.get("/load/agents")
    async def load_agents(_: AdminDep) -> dict:
        return {
            "configured": bool(settings.load_agents and settings.load_token),
            "agents": await loads.agents_view(),
            "mock_available": bool(settings.load_mock_url),
            "live_max_calls": settings.load_live_max_calls,
            "max_duration": settings.load_max_duration,
            "active": loads.active.run.id if loads.active else None,
            "busy": exclusive.holder[1] if exclusive.holder else None,
        }

    @app.post("/load/runs", status_code=202)
    async def start_load(body: LoadRequest, admin: AdminDep) -> dict:
        try:
            run = await loads.start(Starter(id=admin.id, display=admin.display), admin.token, body)
        except Busy as e:
            raise HTTPException(409, str(e)) from None
        except AgentsBusy as e:
            raise HTTPException(409, str(e)) from None
        except LoadInvalid as e:
            raise HTTPException(422, str(e)) from None
        except AgentsUnavailable as e:
            raise HTTPException(502, str(e)) from None
        except SignedOut:
            raise HTTPException(401, "The session has ended; sign in again.") from None
        except NoGrant as e:
            raise HTTPException(502, str(e)) from None
        return run.view()

    @app.get("/load/runs")
    async def load_runs(_: AdminDep) -> dict:
        items = load_store.summaries()
        if loads.active:
            live = loads.active.run.view()
            items = [live if r["id"] == live["id"] else r for r in items]
        return {"items": items, "active": loads.active.run.id if loads.active else None}

    @app.get("/load/runs/{run_id}")
    async def load_run(run_id: str, _: AdminDep) -> dict:
        found = loads.current(run_id)
        if found is None:
            raise HTTPException(404, "There is no such load test.")
        return found.model_dump(mode="json")

    @app.post("/load/runs/{run_id}/cancel", status_code=202)
    async def cancel_load(run_id: str, _: AdminDep) -> dict:
        if not loads.cancel(run_id):
            raise HTTPException(409, "The load test is not going.")
        return {"cancelling": run_id}

    @app.get("/load/runs/{run_id}/events")
    async def load_events(run_id: str, request: Request, _: AdminDep) -> StreamingResponse:
        found = loads.current(run_id)
        if found is None:
            raise HTTPException(404, "There is no such load test.")
        queue = loads.subscribe(run_id)
        return _stream(
            request,
            queue,
            snapshot=lambda: {"event": "snapshot", "run": found.view()},
            end=lambda: {"event": "end", "run": found.view()},
            finished=found.status in LOAD_FINISHED,
            close=lambda q: loads.unsubscribe(run_id, q),
            event=None,
        )

    logging.getLogger("pgw_tester").info(
        "tester ready: core %s / %s, mock %s, %d built-in suites, load agents %s",
        settings.admin_url,
        settings.data_url,
        settings.mock_url or "none",
        len(catalog.builtin),
        ", ".join(a.name for a in settings.load_agents) or "none",
    )
    return app


def _stream(
    request: Request,
    queue: asyncio.Queue | None,
    snapshot: Callable[[], dict],
    end: Callable[[], dict],
    finished: bool,
    close: Callable[[asyncio.Queue], None],
    event: str | None,
) -> StreamingResponse:
    """A run's events as server-sent events: a snapshot, what happens, and the end. `event`
    names every item; None takes the name from the item's own `event`."""

    async def stream() -> AsyncIterator[str]:
        yield _sse("snapshot", snapshot())
        if queue is None or finished:
            yield _sse("end", end())
            return
        try:
            while not await request.is_disconnected():
                try:
                    item = await asyncio.wait_for(queue.get(), KEEPALIVE_SECONDS)
                except TimeoutError:
                    yield ": keep-alive\n\n"
                    continue
                if item is None:
                    yield _sse("end", end())
                    return
                yield _sse(event or item.get("event", "progress"), item)
        finally:
            close(queue)

    return StreamingResponse(
        stream(),
        media_type="text/event-stream",
        headers={"cache-control": "no-cache", "x-accel-buffering": "no"},
    )


def _sse(event: str, data: dict) -> str:
    return f"event: {event}\ndata: {json.dumps(data, default=str)}\n\n"
