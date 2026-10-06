"""Interactive messages: keys, secrets, validation, streams and the preview."""

import json

import httpx
import pytest

from pgw_tester.app import create_app
from pgw_tester.messages import KEY_LIFETIME, Message, MessageRequest, Messenger, SseReader, message_summary
from pgw_tester.store import JsonStore

from .fakecore import ADMIN_TOKEN, VIEWER_TOKEN, FakeCore

OWN_KEY = "pgw_k42_ownsecretownsecretownsecret"
BODY = json.dumps({"model": "acme/gpt", "messages": [{"role": "user", "content": "hi"}]})
SSE = (
    'event: message.start\ndata: {"id": "m"}\n\n'
    ": keep-alive\n\n"
    'event: message.stop\ndata: {"usage": {"cost": {"amount": 0.1}}}\n\n'
)


def messenger(settings, core: FakeCore) -> Messenger:
    store = JsonStore(settings.data_dir / "messages", Message, message_summary)
    return Messenger(settings, store, core.transport())


def request(**more) -> MessageRequest:
    return MessageRequest.model_validate(
        {"org": "o1", "ws": "w1", "path": "/v1/chat/completions", "body": BODY, **more}
    )


def stored(settings) -> str:
    return "".join(f.read_text() for f in (settings.data_dir / "messages").glob("*.json"))


async def test_a_temporary_key_is_made_used_and_deleted(settings, core):
    m = await messenger(settings, core).send("usr_1", "admin", ADMIN_TOKEN, request())
    assert m.exchange.status == 200
    assert m.request_id == "gw_1"
    assert m.plan.status == "ok"
    assert core.previewed[0]["interface"] == "openai"
    assert m.key.kind == "temporary" and m.key.removed is True
    created = next(o for p, o in core.objects.items() if "/keys/" in p) if core.objects else None
    assert created is None, "the key is gone"
    assert f"/orgs/o1/workspaces/w1/keys/{m.key.id}" in core.deleted
    text = stored(settings)
    assert ADMIN_TOKEN not in text
    assert "secretsecretsecret" not in text
    assert m.request_headers["authorization"] == "[redacted]"


async def test_the_temporary_key_expires_by_itself(settings, core):
    seen = []
    original = core.admin

    def spy(req, path):
        if path.endswith("/keys") and req.method == "POST":
            seen.append(json.loads(req.content))
        return original(req, path)

    core.admin = spy
    await messenger(settings, core).send("usr_1", "admin", ADMIN_TOKEN, request())
    assert seen[0]["name"].startswith("tester-trace-")
    assert seen[0]["expires_at"]
    assert KEY_LIFETIME.total_seconds() == 600


@pytest.mark.parametrize("trouble", ["broken", "slow"])
async def test_the_key_is_deleted_when_the_call_fails(settings, core, trouble):
    if trouble == "broken":
        core.broken.add("/v1/chat/completions")
    else:
        core.slow["/v1/chat/completions"] = 3
    m = await messenger(settings, core).send("usr_1", "admin", ADMIN_TOKEN, request(timeout_s=1))
    assert m.exchange.error
    assert m.exchange.status == 0
    assert m.key.removed is True
    assert any("/keys/" in p for p in core.deleted)


async def test_an_own_key_is_used_and_never_stored(settings, core):
    core.keys[OWN_KEY] = "k42"
    draft = {"interface": "openai", "own_key": OWN_KEY, "nested": {"apiKey": OWN_KEY}, "model": "acme/gpt"}
    m = await messenger(settings, core).send(
        "usr_1",
        "admin",
        ADMIN_TOKEN,
        request(key={"kind": "own", "secret": OWN_KEY}, auth_header="x-api-key", draft=draft),
    )
    assert m.exchange.status == 200
    assert m.key.kind == "own" and m.key.id == "k42" and m.key.removed is None
    assert not any("/keys" in p for _, p in core.calls if "/keys" in p and "workspaces" in p)
    assert m.draft == {"interface": "openai", "nested": {}, "model": "acme/gpt"}
    assert "ownsecret" not in stored(settings)


async def test_a_secret_in_the_draft_drops_the_draft(settings, core):
    core.keys[OWN_KEY] = "k42"
    m = await messenger(settings, core).send(
        "usr_1",
        "admin",
        ADMIN_TOKEN,
        request(key={"kind": "own", "secret": OWN_KEY}, draft={"note": f"key is {OWN_KEY}"}),
    )
    assert m.draft is None


async def test_a_wrong_own_key_is_answered_not_hidden(settings, core):
    m = await messenger(settings, core).send(
        "usr_1", "admin", ADMIN_TOKEN, request(key={"kind": "own", "secret": "pgw_x_wrongwrongwrong"})
    )
    assert m.exchange.status == 401


async def test_streams_are_split_into_timed_events(settings, core):
    core.data["/v1/stream"] = (200, {"content-type": "text/event-stream"}, SSE)
    m = await messenger(settings, core).send("usr_1", "admin", ADMIN_TOKEN, request(path="/v1/stream"))
    assert m.exchange.stream
    assert [e.event for e in m.exchange.events] == ["message.start", "message.stop"]
    assert m.exchange.first_byte_ms is not None
    assert m.plan.status == "skipped"


def test_sse_reader_handles_split_chunks():
    reader = SseReader()
    reader.feed("event: a\nda", 1)
    reader.feed('ta: {"x":1}\n\nevent: b\r\ndata: 2\r\n\r\n', 2)
    reader.feed("data: [DONE]", 3)
    reader.finish(4)
    assert [(e.event, e.data, e.t_ms) for e in reader.events] == [
        ("a", '{"x":1}', 2),
        ("b", "2", 2),
        ("", "[DONE]", 4),
    ]


async def test_the_preview_reports_a_refusal_and_the_body_is_sent_anyway(settings, core):
    core.preview = (
        422,
        {
            "error": {
                "type": "model_not_found",
                "message": "no such model",
                "details": {"gateway_status": 404},
            }
        },
    )
    m = await messenger(settings, core).send("usr_1", "admin", ADMIN_TOKEN, request())
    assert m.plan.status == "refused" and m.plan.gateway_status == 404
    assert m.exchange.status == 200


async def test_only_gateway_headers_go_to_the_preview(settings, core):
    await messenger(settings, core).send(
        "usr_1",
        "admin",
        ADMIN_TOKEN,
        request(headers={"X-GW-End-User": "u1", "anthropic-version": "2023-06-01", "x-custom": "1"}),
    )
    assert core.previewed[0]["headers"] == {"x-gw-end-user": "u1", "anthropic-version": "2023-06-01"}


async def test_a_body_that_is_not_json_is_sent_as_it_is(settings, core):
    m = await messenger(settings, core).send("usr_1", "admin", ADMIN_TOKEN, request(body="{not json"))
    assert m.plan.status == "skipped" and "not JSON" in m.plan.detail
    assert m.body == "{not json"


@pytest.mark.parametrize(
    "bad",
    [
        {"path": "http://evil.example/x"},
        {"path": "//evil.example/x"},
        {"path": "@evil.example/x"},
        {"path": "/v1/x y"},
        {"path": "/v1\\x"},
        {"headers": {"Authorization": "Bearer x"}},
        {"headers": {"x-api-key": "x"}},
        {"headers": {"Host": "evil"}},
        {"headers": {"x-gw-end-user": "a\r\nx-evil: 1"}},
        {"headers": {"bad header": "1"}},
        {"org": "../x"},
        {"key": {"kind": "own"}},
        {"timeout_s": 900},
    ],
)
def test_requests_that_could_escape_are_refused(bad):
    with pytest.raises(ValueError):
        request(**bad)


@pytest.fixture
async def client(settings, core: FakeCore):
    app = create_app(settings, core.transport())
    async with httpx.AsyncClient(transport=httpx.ASGITransport(app=app), base_url="http://tester") as c:
        yield c


async def test_messages_through_the_api(client):
    admin = {"authorization": f"Bearer {ADMIN_TOKEN}"}
    payload = {"org": "o1", "ws": "w1", "path": "/v1/chat/completions", "body": BODY}
    assert (await client.post("/messages", json=payload)).status_code == 401
    viewer = {"authorization": f"Bearer {VIEWER_TOKEN}"}
    assert (await client.post("/messages", json=payload, headers=viewer)).status_code == 403
    assert (await client.get("/messages", headers=viewer)).status_code == 403

    r = await client.post("/messages", json=payload, headers=admin)
    assert r.status_code == 201, r.text
    message_id = r.json()["id"]
    listed = (await client.get("/messages", headers=admin)).json()["items"]
    assert listed[0]["id"] == message_id and listed[0]["status"] == 200
    assert (await client.get(f"/messages/{message_id}", headers=admin)).json()["request_id"] == "gw_1"
    assert (await client.get("/messages/20260101-000000-000000", headers=admin)).status_code == 404

    bad = await client.post("/messages", json={**payload, "path": "http://evil"}, headers=admin)
    assert bad.status_code == 422
    assert any("path" in e for e in bad.json()["errors"])
