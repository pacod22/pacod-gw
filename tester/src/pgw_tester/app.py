"""The tester's HTTP API. Its only client is the server side of the console."""

import asyncio
import contextlib
import json
import logging
from collections.abc import AsyncIterator
from typing import Annotated

import httpx
from fastapi import Depends, FastAPI, HTTPException, Request, Response
from fastapi.exceptions import RequestValidationError
from fastapi.responses import JSONResponse, StreamingResponse

from .auth import Admin, SessionCheck
from .catalog import Catalog, CatalogError
from .config import Settings
from .engine import Busy, Invalid, Runner
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
    runner = Runner(settings, catalog, store, transport)
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

    app = FastAPI(title="pacod-gw tester", version="0.1.0", lifespan=lifespan, docs_url=None, redoc_url=None)
    app.state.runner = runner

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

        async def stream() -> AsyncIterator[str]:
            yield _sse("snapshot", {"event": "snapshot", "run": found.summary()})
            if queue is None or found.status in FINISHED:
                yield _sse("end", {"event": "end", "run": found.summary()})
                return
            try:
                while not await request.is_disconnected():
                    try:
                        item = await asyncio.wait_for(queue.get(), KEEPALIVE_SECONDS)
                    except TimeoutError:
                        yield ": keep-alive\n\n"
                        continue
                    if item is None:
                        yield _sse("end", {"event": "end", "run": found.summary()})
                        return
                    yield _sse("progress", item)
            finally:
                runner.unsubscribe(run_id, queue)

        return StreamingResponse(
            stream(),
            media_type="text/event-stream",
            headers={"cache-control": "no-cache", "x-accel-buffering": "no"},
        )

    logging.getLogger("pgw_tester").info(
        "tester ready: core %s / %s, mock %s, %d built-in suites",
        settings.admin_url,
        settings.data_url,
        settings.mock_url or "none",
        len(catalog.builtin),
    )
    return app


def _sse(event: str, data: dict) -> str:
    return f"event: {event}\ndata: {json.dumps(data, default=str)}\n\n"
