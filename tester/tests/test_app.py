"""The HTTP API: who may use it, and the run life cycle through it."""

import httpx
import pytest

from pgw_tester.app import create_app

from .fakecore import ADMIN_TOKEN, VIEWER_TOKEN, FakeCore


@pytest.fixture
async def client(settings, core: FakeCore):
    app = create_app(settings, core.transport())
    async with httpx.AsyncClient(transport=httpx.ASGITransport(app=app), base_url="http://tester") as c:
        c.app = app
        yield c


def auth(token: str) -> dict[str, str]:
    return {"authorization": f"Bearer {token}"}


async def test_health_needs_nothing(client):
    assert (await client.get("/healthz")).status_code == 200


@pytest.mark.parametrize("path", ["/catalog", "/runs", "/runs/20260101-000000-abcdef", "/cases/health"])
async def test_only_platform_admins_get_in(client, path):
    assert (await client.get(path)).status_code == 401
    assert (await client.get(path, headers=auth("pgws_unknown_0123456789"))).status_code == 401
    assert (await client.get(path, headers=auth(VIEWER_TOKEN))).status_code == 403


async def test_writes_need_an_admin_too(client):
    assert (await client.post("/runs", json={"mode": "mock"}, headers=auth(VIEWER_TOKEN))).status_code == 403
    assert (await client.post("/cases", content="x", headers=auth(VIEWER_TOKEN))).status_code == 403
    assert (await client.delete("/cases/x", headers=auth(VIEWER_TOKEN))).status_code == 403


async def test_catalog_lists_the_builtin_suites(client):
    r = await client.get("/catalog", headers=auth(ADMIN_TOKEN))
    assert r.status_code == 200
    body = r.json()
    assert body["mock_available"] is True
    assert body["suites"][0]["suite"] == "health"
    assert (await client.get("/cases/health", headers=auth(ADMIN_TOKEN))).text.startswith("suite: health")


async def test_upload_reports_problems(client):
    r = await client.post("/cases", content="suite: x\ntitle: x\ncases: []", headers=auth(ADMIN_TOKEN))
    assert r.status_code == 422
    assert r.json()["errors"]


async def test_a_run_through_the_api(client):
    r = await client.post(
        "/runs", json={"mode": "mock", "cases": ["data-plane-live"]}, headers=auth(ADMIN_TOKEN)
    )
    assert r.status_code == 202, r.text
    run_id = r.json()["id"]
    assert r.json()["started_by"]["display"] == "admin@example.com"
    await client.app.state.runner.wait()

    run = (await client.get(f"/runs/{run_id}", headers=auth(ADMIN_TOKEN))).json()
    assert run["status"] == "passed"
    listed = (await client.get("/runs", headers=auth(ADMIN_TOKEN))).json()
    assert listed["items"][0]["id"] == run_id
    assert "suites" not in listed["items"][0]

    events = await client.get(f"/runs/{run_id}/events", headers=auth(ADMIN_TOKEN))
    assert "event: end" in events.text
    assert (await client.post(f"/runs/{run_id}/cancel", headers=auth(ADMIN_TOKEN))).status_code == 409


async def test_invalid_runs_are_refused(client):
    r = await client.post("/runs", json={"mode": "live"}, headers=auth(ADMIN_TOKEN))
    assert r.status_code == 422
    r = await client.post("/runs", json={"mode": "sometimes"}, headers=auth(ADMIN_TOKEN))
    assert r.status_code == 422
