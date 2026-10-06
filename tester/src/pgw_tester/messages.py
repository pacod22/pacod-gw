"""Interactive messages: one call to the data plane, composed by an admin, sent and recorded with
the timing of every byte, so that the console can trace it through the gateway.

A message is sent with a key of the chosen workspace: a temporary one that the tester creates
for this message alone and deletes afterwards (it also expires after ten minutes, should the
tester stop in between), or a key the admin pastes. Neither is ever stored.
"""

import asyncio
import json
import re
import secrets
import time
from datetime import timedelta
from typing import Any, Literal

import httpx
from pydantic import BaseModel, Field, field_validator, model_validator

from .config import Settings
from .redact import Redactor
from .results import Starter, now
from .store import JsonStore

#: How much of a response is kept.
BODY_LIMIT = 256 * 1024
#: How much of the request body is kept.
REQUEST_LIMIT = 64 * 1024
#: Events of a stream that are kept, and how much of each.
EVENT_LIMIT = 2000
EVENT_DATA_LIMIT = 2000
#: Messages that may be in flight at once.
CONCURRENT = 4
#: A temporary key stops working by itself after this long.
KEY_LIFETIME = timedelta(minutes=10)

ID = re.compile(r"^[A-Za-z0-9_-]{1,100}$")
#: Headers the tester sets itself, or that would let a message escape its target.
RESERVED_HEADERS = {
    "authorization",
    "x-api-key",
    "x-goog-api-key",
    "proxy-authorization",
    "cookie",
    "host",
    "content-length",
    "transfer-encoding",
    "connection",
    "upgrade",
    "te",
}
_HEADER_NAME = re.compile(r"^[!#$%&'*+.^_`|~0-9A-Za-z-]{1,100}$")
#: The managed interfaces the playground can preview, by path.
PREVIEWABLE = {"/v1/chat/completions": "openai", "/v1/messages": "anthropic", "/gw/v1/chat": "native"}
#: Fields of the composer's saved state that could hold a secret; they are never stored.
_SECRET_KEYS = re.compile(r"secret|password|token|own_?key|api_?key", re.IGNORECASE)


class KeyChoice(BaseModel):
    kind: Literal["temporary", "own"] = "temporary"
    #: The admin's own key, for kind `own`. Used for this message only.
    secret: str | None = Field(default=None, repr=False)

    @model_validator(mode="after")
    def _secret(self) -> "KeyChoice":
        if self.kind == "own" and not (self.secret or "").strip():
            raise ValueError("paste the API key to send the message with")
        if self.kind == "temporary":
            self.secret = None
        return self


class MessageRequest(BaseModel):
    org: str
    ws: str
    key: KeyChoice = KeyChoice()
    method: Literal["GET", "POST", "PUT", "PATCH", "DELETE"] = "POST"
    #: The data plane path, with any query, such as `/v1/chat/completions`.
    path: str
    headers: dict[str, str] = {}
    #: Where the key goes: `authorization` (as a bearer token), `x-api-key` or `x-goog-api-key`.
    auth_header: Literal["authorization", "x-api-key", "x-goog-api-key"] = "authorization"
    #: The body exactly as it is sent; it need not be valid JSON.
    body: str | None = None
    timeout_s: float = Field(default=120.0, ge=1, le=300)
    #: The composer's state, kept so that the message can be edited and sent again.
    draft: dict[str, Any] | None = None

    @field_validator("org", "ws")
    @classmethod
    def _id(cls, value: str) -> str:
        if not ID.match(value):
            raise ValueError("not an id")
        return value

    @field_validator("path")
    @classmethod
    def _path(cls, value: str) -> str:
        if not value.startswith("/") or value.startswith("//"):
            raise ValueError("the path starts with a single /")
        if "://" in value or "\\" in value or any(c.isspace() or ord(c) < 32 or ord(c) == 127 for c in value):
            raise ValueError("the path may not hold a scheme, backslashes, spaces or control characters")
        if len(value) > 2000:
            raise ValueError("the path is too long")
        return value

    @field_validator("headers")
    @classmethod
    def _headers(cls, value: dict[str, str]) -> dict[str, str]:
        out: dict[str, str] = {}
        for name, v in value.items():
            name = name.strip()
            if not _HEADER_NAME.match(name):
                raise ValueError(f"{name!r} is not a header name")
            if name.lower() in RESERVED_HEADERS:
                raise ValueError(f"the header {name} is set by the tester; choose where the key goes instead")
            if "\r" in v or "\n" in v or "\0" in v:
                raise ValueError(f"the value of {name} holds a line break")
            out[name] = v
        return out

    @field_validator("body")
    @classmethod
    def _body(cls, value: str | None) -> str | None:
        if value is not None and len(value.encode()) > 4 * 1024 * 1024:
            raise ValueError("the body is larger than 4 MiB")
        return value


class StreamEvent(BaseModel):
    #: Milliseconds after the request was sent.
    t_ms: float
    event: str = ""
    data: str = ""


class Exchange(BaseModel):
    status: int = 0
    headers: dict[str, str] = {}
    body: str = ""
    body_bytes: int = 0
    truncated: bool = False
    #: Until the response head arrived.
    ttfb_ms: float | None = None
    #: Until the first byte of the body arrived.
    first_byte_ms: float | None = None
    total_ms: float | None = None
    chunks: int = 0
    stream: bool = False
    events: list[StreamEvent] = []
    #: Why there is no answer: the connection failed or the time ran out.
    error: str = ""


class Plan(BaseModel):
    """What the gateway intends to send, from the workspace's playground preview."""

    status: Literal["ok", "refused", "skipped", "error"]
    detail: str = ""
    gateway_status: int | None = None
    preview: dict[str, Any] | None = None


class KeyUse(BaseModel):
    kind: Literal["temporary", "own"]
    #: The temporary key's id, or the id part of a pasted key.
    id: str = ""
    #: Whether the temporary key was deleted afterwards.
    removed: bool | None = None
    problem: str = ""


class Message(BaseModel):
    id: str
    created_at: Any = Field(default_factory=now)
    started_by: Starter
    org: str
    ws: str
    key: KeyUse
    method: str
    path: str
    url: str
    request_headers: dict[str, str]
    body: str | None
    body_bytes: int
    plan: Plan
    exchange: Exchange
    #: The gateway's id of the call, from x-gw-request-id: the key to its log entry and audit record.
    request_id: str = ""
    draft: dict[str, Any] | None = None


def message_summary(data: dict[str, Any]) -> dict[str, Any]:
    ex = data.get("exchange") or {}
    headers = ex.get("headers") or {}
    return {
        "id": data["id"],
        "created_at": data.get("created_at"),
        "started_by": data.get("started_by"),
        "org": data.get("org"),
        "ws": data.get("ws"),
        "method": data.get("method"),
        "path": data.get("path"),
        "status": ex.get("status", 0),
        "error": ex.get("error", ""),
        "total_ms": ex.get("total_ms"),
        "request_id": data.get("request_id", ""),
        "model_served": headers.get("x-gw-model-served", ""),
    }


class Busy(Exception):
    pass


def _clip(text: str, limit: int) -> tuple[str, bool]:
    return (text, False) if len(text) <= limit else (text[:limit], True)


def _key_id(secret: str) -> str:
    """The id part of a gateway key (`pgw_<id>_<secret>`), which is not secret."""
    parts = secret.split("_")
    return parts[1] if len(parts) >= 3 and parts[0] == "pgw" else ""


def _clean_draft(draft: dict[str, Any] | None, redact: Redactor) -> dict[str, Any] | None:
    """The composer's state without anything that could be a secret."""
    if draft is None:
        return None

    def strip(value: Any) -> Any:
        if isinstance(value, dict):
            return {k: strip(v) for k, v in value.items() if not _SECRET_KEYS.search(str(k))}
        if isinstance(value, list):
            return [strip(v) for v in value]
        return value

    cleaned = strip(draft)
    text = json.dumps(cleaned)
    # A secret pasted into some other field: keep nothing rather than the secret.
    return None if redact.leaks(text) or redact(text) != text else cleaned


class SseReader:
    """Splits a byte stream into server-sent events, noting when each arrived."""

    def __init__(self) -> None:
        self.buffer = ""
        self.events: list[StreamEvent] = []
        self.dropped = 0

    def feed(self, text: str, t_ms: float) -> None:
        self.buffer += text.replace("\r\n", "\n")
        while "\n\n" in self.buffer:
            block, self.buffer = self.buffer.split("\n\n", 1)
            self._event(block, t_ms)

    def finish(self, t_ms: float) -> None:
        if self.buffer.strip():
            self._event(self.buffer, t_ms)
        self.buffer = ""

    def _event(self, block: str, t_ms: float) -> None:
        name, data = "", []
        for line in block.split("\n"):
            if line.startswith("event:"):
                name = line[6:].strip()
            elif line.startswith("data:"):
                data.append(line[5:].lstrip())
        if not name and not data:
            return  # a comment or keep-alive
        if len(self.events) >= EVENT_LIMIT:
            self.dropped += 1
            return
        text, _ = _clip("\n".join(data), EVENT_DATA_LIMIT)
        self.events.append(StreamEvent(t_ms=round(t_ms, 1), event=name, data=text))


class Messenger:
    def __init__(
        self,
        settings: Settings,
        store: JsonStore[Message],
        transport: httpx.AsyncBaseTransport | None = None,
    ):
        self.settings = settings
        self.store = store
        self.transport = transport
        self.in_flight = 0

    async def send(self, admin_id: str, admin_display: str, token: str, req: MessageRequest) -> Message:
        if self.in_flight >= CONCURRENT:
            raise Busy(f"{CONCURRENT} messages are on their way already; wait for one of them")
        self.in_flight += 1
        try:
            return await self._send(Starter(id=admin_id, display=admin_display), token, req)
        finally:
            self.in_flight -= 1

    async def _send(self, starter: Starter, token: str, req: MessageRequest) -> Message:
        message_id = now().strftime("%Y%m%d-%H%M%S") + "-" + secrets.token_hex(3)
        redact = Redactor([token])
        if req.key.secret:
            redact.add(req.key.secret)
        api = f"{self.settings.admin_url}/api/v1/orgs/{req.org}/workspaces/{req.ws}"
        session = {"authorization": f"Bearer {token}"}

        async with httpx.AsyncClient(transport=self.transport, follow_redirects=False) as http:
            plan = await self._plan(http, api, session, req, redact)
            key = KeyUse(kind=req.key.kind)
            secret = req.key.secret or ""
            if req.key.kind == "own":
                key.id = _key_id(secret)
            else:
                secret, key = await self._temporary_key(http, api, session, message_id, redact)

            headers = dict(req.headers)
            if secret:
                headers[req.auth_header] = (
                    f"Bearer {secret}" if req.auth_header == "authorization" else secret
                )
            if req.body is not None and not any(k.lower() == "content-type" for k in headers):
                headers["content-type"] = "application/json"
            url = self.settings.data_url + req.path
            try:
                exchange = (
                    await self._exchange(http, req, url, headers, redact)
                    if secret
                    else Exchange(error="no key to send the message with")
                )
            finally:
                if req.key.kind == "temporary" and key.id:
                    await self._remove_key(http, api, session, key, redact)

        body, _ = _clip(redact(req.body), REQUEST_LIMIT) if req.body is not None else (None, False)
        message = Message(
            id=message_id,
            started_by=starter,
            org=req.org,
            ws=req.ws,
            key=key,
            method=req.method,
            path=redact(req.path),
            url=redact(url),
            request_headers=redact.headers(headers),
            body=body,
            body_bytes=len(req.body.encode()) if req.body is not None else 0,
            plan=plan,
            exchange=exchange,
            request_id=exchange.headers.get("x-gw-request-id", ""),
            draft=_clean_draft(req.draft, redact),
        )
        self.store.save(message)
        self.store.prune()
        return message

    async def _plan(
        self,
        http: httpx.AsyncClient,
        api: str,
        session: dict[str, str],
        req: MessageRequest,
        redact: Redactor,
    ) -> Plan:
        """The playground's preview of what the gateway would send, for the managed interfaces."""
        interface = PREVIEWABLE.get(req.path.split("?", 1)[0])
        if interface is None or req.method != "POST":
            return Plan(status="skipped", detail="a preview exists for the managed interfaces only")
        try:
            parsed = json.loads(req.body or "")
        except ValueError:
            return Plan(status="skipped", detail="the body is not JSON")
        if not isinstance(parsed, dict):
            return Plan(status="skipped", detail="the body is not a JSON object")
        taken = {
            k.lower(): v for k, v in req.headers.items() if k.lower().startswith(("x-gw-", "anthropic-"))
        }
        try:
            r = await http.post(
                f"{api}/playground/transform",
                headers=session,
                json={"interface": interface, "request": parsed, "headers": taken},
                timeout=15.0,
            )
        except httpx.HTTPError as e:
            return Plan(status="error", detail=f"the preview could not be asked for: {type(e).__name__}")
        data = _json(r)
        if r.status_code == 200 and isinstance(data, dict):
            return Plan(status="ok", preview=json.loads(redact(json.dumps(data))))
        error = data.get("error", {}) if isinstance(data, dict) else {}
        message = redact(str(error.get("message") or r.text[:500]))
        if r.status_code == 422:
            details = error.get("details") or {}
            return Plan(
                status="refused",
                detail=f"{error.get('type', 'refused')}: {message}",
                gateway_status=details.get("gateway_status") if isinstance(details, dict) else None,
            )
        return Plan(status="error", detail=f"the preview answered {r.status_code}: {message}")

    async def _temporary_key(
        self, http: httpx.AsyncClient, api: str, session: dict[str, str], message_id: str, redact: Redactor
    ) -> tuple[str, KeyUse]:
        key = KeyUse(kind="temporary")
        body = {"name": f"tester-trace-{message_id}", "expires_at": (now() + KEY_LIFETIME).isoformat()}
        try:
            r = await http.post(f"{api}/keys", headers=session, json=body, timeout=15.0)
        except httpx.HTTPError as e:
            key.problem = f"the temporary key could not be created: {type(e).__name__}"
            return "", key
        data = _json(r)
        if r.status_code != 201 or not isinstance(data, dict) or not data.get("secret"):
            key.problem = f"the temporary key could not be created ({r.status_code}): {redact(r.text[:300])}"
            return "", key
        redact.add(data["secret"])
        key.id = str(data.get("id", ""))
        return data["secret"], key

    async def _remove_key(
        self, http: httpx.AsyncClient, api: str, session: dict[str, str], key: KeyUse, redact: Redactor
    ) -> None:
        try:
            r = await http.delete(f"{api}/keys/{key.id}", headers=session, timeout=15.0)
            key.removed = r.status_code in (204, 404)
            if not key.removed:
                key.problem = f"the temporary key was not deleted ({r.status_code}); it expires by itself"
        except httpx.HTTPError as e:
            key.removed = False
            key.problem = f"the temporary key was not deleted ({type(e).__name__}); it expires by itself"
        key.problem = redact(key.problem)

    async def _exchange(
        self,
        http: httpx.AsyncClient,
        req: MessageRequest,
        url: str,
        headers: dict[str, str],
        redact: Redactor,
    ) -> Exchange:
        ex = Exchange()
        content = req.body.encode() if req.body is not None else None
        started = time.monotonic()
        elapsed = lambda: round((time.monotonic() - started) * 1000, 1)  # noqa: E731
        received = bytearray()
        sse = SseReader()
        decoder = None
        try:
            async with asyncio.timeout(req.timeout_s):
                async with http.stream(
                    req.method, url, headers=headers, content=content, timeout=req.timeout_s
                ) as r:
                    ex.ttfb_ms = elapsed()
                    ex.status = r.status_code
                    ex.headers = {k: redact(v) for k, v in r.headers.items()}
                    ex.stream = "text/event-stream" in r.headers.get("content-type", "")
                    decoder = r.encoding or "utf-8"
                    async for chunk in r.aiter_bytes():
                        t = elapsed()
                        if ex.first_byte_ms is None:
                            ex.first_byte_ms = t
                        ex.chunks += 1
                        ex.body_bytes += len(chunk)
                        if len(received) < BODY_LIMIT:
                            received.extend(chunk[: BODY_LIMIT - len(received)])
                        else:
                            ex.truncated = True
                        if ex.stream:
                            sse.feed(chunk.decode(decoder, errors="replace"), t)
        except TimeoutError:
            ex.error = f"no complete answer within {req.timeout_s:g} s"
        except httpx.HTTPError as e:
            ex.error = redact(f"{type(e).__name__}: {e}".strip())
        ex.total_ms = elapsed()
        if ex.stream:
            sse.finish(ex.total_ms)
            ex.events = [StreamEvent(t_ms=e.t_ms, event=e.event, data=redact(e.data)) for e in sse.events]
        ex.body = redact(received.decode(decoder or "utf-8", errors="replace"))
        return ex


def _json(r: httpx.Response) -> Any:
    try:
        return r.json()
    except ValueError:
        return None
