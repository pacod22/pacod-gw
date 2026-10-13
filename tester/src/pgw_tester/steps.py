"""Running one step: render it, send it, check the answer, keep what it saves."""

import asyncio
import json
import time
from dataclasses import dataclass, field
from typing import Any
from urllib.parse import urlencode

import httpx
from pacod_id.grant import DelegatedGrant, SignedOut

from .config import Settings
from .expect import Reply, evaluate
from .model import Expect, Step
from .redact import Redactor
from .results import Received, Sent, StepResult
from .templating import MissingVariable, get_path, render

#: How much of a body a step result keeps.
BODY_LIMIT = 6000
#: Response headers a step result keeps.
KEPT_HEADERS = ("content-type", "retry-after", "location")
#: A value saved from a field of this name is a secret from then on.
SECRET_FIELDS = {"secret", "token", "key", "password"}
#: How long a change of the configuration may take to reach every gateway instance.
APPLY_WAIT = 5.0


class Cancelled(Exception):
    pass


@dataclass
class Context:
    """What the steps of one suite share."""

    settings: Settings
    http: httpx.AsyncClient
    redact: Redactor
    #: The admin's session token, for `auth: session`; with a delegated grant, its current
    #: access token (see `session_token`).
    token: str
    cancel: asyncio.Event
    #: The run's delegated grant of the admin's session (pacod-id), when there is one.
    grant: DelegatedGrant | None = None
    variables: dict[str, Any] = field(default_factory=dict)
    #: Paths of objects created in this suite, deleted in reverse order afterwards.
    ledger: list[str] = field(default_factory=list)
    #: What is available to `needs`.
    available: set[str] = field(default_factory=set)

    async def session_token(self) -> str:
        """The admin's token for the next call: the grant's, refreshed when due."""
        if self.grant is not None:
            self.token = await self.grant.access_token()
            self.redact.add(self.token)
        return self.token

    async def pause(self, seconds: float) -> None:
        """Sleeps, unless the run is cancelled first."""
        if seconds <= 0:
            return
        try:
            await asyncio.wait_for(self.cancel.wait(), timeout=seconds)
        except TimeoutError:
            return
        raise Cancelled()

    async def await_applied(self, limit: float = APPLY_WAIT) -> None:
        """Waits until every running gateway instance has applied the configuration version
        the management API is at. With several instances a change reaches the others within a
        second or two, and the next call may land on any of them. Gives up quietly after
        `limit` seconds, or when the core does not say."""
        url = f"{self.settings.admin_url}/api/v1/monitoring/live"
        await self.session_token()
        deadline = time.monotonic() + limit
        while time.monotonic() < deadline:
            try:
                response = await self.http.get(
                    url, headers={"authorization": f"Bearer {self.token}"}, timeout=5
                )
            except httpx.HTTPError:
                return
            if response.status_code != 200:
                return
            live = response.json()
            version, instances = live.get("config_version"), live.get("instances")
            if not isinstance(version, int) or not isinstance(instances, list):
                return
            if all(
                isinstance(i.get("applied_version"), int) and i["applied_version"] >= version
                for i in instances
            ):
                return
            await self.pause(0.1)


def _clip(text: str) -> str:
    return (
        text
        if len(text) <= BODY_LIMIT
        else text[:BODY_LIMIT] + f"\n… ({len(text) - BODY_LIMIT} more characters)"
    )


def _url(ctx: Context, step: Step, path: str, query: dict[str, Any]) -> str:
    request = step.request
    assert request is not None
    if step.kind == "mgmt":
        base = ctx.settings.admin_url + ("/api/v1" if request.base == "api" else "")
    elif step.kind == "call":
        base = ctx.settings.data_url
    else:
        if not ctx.settings.mock_control_url:
            raise LookupError("no mock provider is configured (TESTER_MOCK_CONTROL_URL)")
        base = ctx.settings.mock_control_url
    url = base + path
    if query:
        url += ("&" if "?" in url else "?") + urlencode({k: _plain(v) for k, v in query.items()})
    return url


def _plain(value: Any) -> str:
    if isinstance(value, bool):
        return "true" if value else "false"
    return str(value)


def _auth(ctx: Context, step: Step, variables: dict[str, Any]) -> dict[str, str]:
    request = step.request
    assert request is not None
    auth = request.auth or {"mgmt": "session", "call": "bearer", "mock": "none"}[step.kind]
    if auth == "none":
        return {}
    if auth == "session":
        return {"authorization": f"Bearer {ctx.token}"}
    key = str(render(request.key, variables))
    return {"authorization": f"Bearer {key}"} if auth == "bearer" else {"x-api-key": key}


def _saved(source: str, reply: Reply) -> tuple[bool, Any]:
    if source == "status":
        return True, reply.status
    if source.startswith("header."):
        value = reply.headers.get(source[7:])
        return value is not None, value
    return get_path(reply.json(), source[5:])


def _strip_query(path: str) -> str:
    return path.split("?", 1)[0].rstrip("/")


async def run_step(ctx: Context, step: Step, variables: dict[str, Any]) -> StepResult:
    """Runs a step with the given variables; what it saves is written into them."""
    result = StepResult(name=step.label(), kind=step.kind, status="passed")
    missing = [n for n in step.needs if n not in ctx.available]
    if missing:
        result.status = "skipped"
        result.detail = f"needs {', '.join(missing)}"
        return result
    started = time.monotonic()
    try:
        if step.sleep is not None:
            await ctx.pause(step.sleep)
            result.detail = f"waited {step.sleep:g} s"
            return result
        await ctx.session_token()
        await _exchange(ctx, step, variables, result)
    except (Cancelled, SignedOut):
        raise
    except MissingVariable as e:
        result.status = "error"
        result.failures = [str(e)]
    except LookupError as e:
        result.status = "error"
        result.failures = [str(e)]
    finally:
        result.duration_ms = round((time.monotonic() - started) * 1000, 1)
    return result


async def _exchange(ctx: Context, step: Step, variables: dict[str, Any], result: StepResult) -> None:
    request = step.request
    assert request is not None
    method = request.method
    path = str(render(request.path, variables))
    url = _url(ctx, step, path, render(request.query, variables))
    headers = {k: str(v) for k, v in render(request.headers, variables).items()}
    headers.update(_auth(ctx, step, variables))
    body = render(request.body, variables)
    content: bytes | None = None
    if body is not None:
        if isinstance(body, str):
            content = body.encode()
        else:
            content = json.dumps(body).encode()
            headers.setdefault("content-type", "application/json")
    result.request = Sent(
        method=method,
        url=ctx.redact(url),
        headers=ctx.redact.headers(headers),
        body=_clip(ctx.redact(content.decode())) if content else None,
    )

    expect = Expect.model_validate(render(step.expect.model_dump(by_alias=True), variables))
    deadline = time.monotonic() + step.retry.within if step.retry else 0.0
    reply: Reply | None = None
    failures: list[str] = []
    while True:
        if ctx.cancel.is_set():
            raise Cancelled()
        result.attempts += 1
        sent = time.monotonic()
        try:
            response = await ctx.http.request(
                method, url, headers=headers, content=content, timeout=request.timeout
            )
            reply = Reply(
                status=response.status_code,
                headers=response.headers,
                text=response.text,
                elapsed_ms=round((time.monotonic() - sent) * 1000, 1),
            )
            failures = evaluate(expect, reply)
            leaked = ctx.redact.leaks(reply.text) or any(ctx.redact.leaks(v) for v in reply.headers.values())
            if leaked:
                failures.append("the answer contains a secret of this run")
        except httpx.HTTPError as e:
            reply = None
            failures = [f"{method} {ctx.redact(url)} failed: {type(e).__name__} {ctx.redact(str(e))}".strip()]
        if not failures or not step.retry or time.monotonic() >= deadline:
            break
        await ctx.pause(step.retry.every)

    if reply is not None:
        kept = {
            k: ctx.redact(v) for k, v in reply.headers.items() if k.startswith("x-gw-") or k in KEPT_HEADERS
        }
        result.response = Received(
            status=reply.status, headers=kept, body=_clip(ctx.redact(reply.text)), elapsed_ms=reply.elapsed_ms
        )
    if failures:
        result.status = "failed"
        result.failures = [ctx.redact(f) for f in failures]
        return

    assert reply is not None
    for name, source in step.save.items():
        found, value = _saved(source, reply)
        if not found:
            result.status = "failed"
            result.failures.append(f"cannot save {name}: the answer has no {source}")
            continue
        variables[name] = value
        if source.rsplit(".", 1)[-1].lower() in SECRET_FIELDS and isinstance(value, str):
            ctx.redact.add(value)

    if step.kind == "mgmt" and request.base == "api" and method != "GET" and 200 <= reply.status < 300:
        await ctx.await_applied()

    created = reply.json() if step.kind == "mgmt" and method == "POST" and reply.status == 201 else None
    if step.cleanup and isinstance(created, dict) and isinstance(created.get("id"), str):
        ctx.ledger.append(f"{_strip_query(path)}/{created['id']}")

    gw = reply.headers.get("x-gw-request-id")
    attempts = f", {result.attempts} attempts" if result.attempts > 1 else ""
    result.detail = f"{reply.status} in {reply.elapsed_ms:g} ms{attempts}" + (f", request {gw}" if gw else "")
