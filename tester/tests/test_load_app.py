"""The load test routes: who may use them, and a load test through them."""

import dataclasses

import httpx
import pytest

from pgw_tester.app import create_app
from pgw_tester.load import runner as load_runner
from pgw_tester.load.agents import AgentRef

from .fakeagent import LOAD_TOKEN, FakeAgent, transport
from .fakecore import ADMIN_TOKEN, VIEWER_TOKEN, FakeCore

BODY = {
    "target": "mock",
    "mock": {"delay_ms": 10},
    "load": {"stages": [{"duration_s": 0, "target": 2}, {"duration_s": 2, "target": 2}]},
    "agents": ["a"],
}


@pytest.fixture
async def client(settings, core: FakeCore, monkeypatch):
    monkeypatch.setattr(load_runner, "POLL_EVERY", 0.01)
    monkeypatch.setattr(load_runner, "STALE_AFTER", 0.05)
    agent = FakeAgent("a", seconds=2)
    s = dataclasses.replace(
        settings,
        load_agents=(AgentRef("a", "http://agent-a"),),
        load_token=LOAD_TOKEN,
        load_mock_url="http://load-mock",
    )
    app = create_app(s, transport(core, [agent]))
    async with httpx.AsyncClient(transport=httpx.ASGITransport(app=app), base_url="http://tester") as c:
        c.app = app
        yield c


def auth(token: str) -> dict[str, str]:
    return {"authorization": f"Bearer {token}"}


@pytest.mark.parametrize("path", ["/load/agents", "/load/runs", "/load/runs/20260101-000000-abcdef"])
async def test_only_platform_admins_get_in(client, path):
    assert (await client.get(path)).status_code == 401
    assert (await client.get(path, headers=auth(VIEWER_TOKEN))).status_code == 403
    r = await client.post("/load/runs", json=BODY, headers=auth(VIEWER_TOKEN))
    assert r.status_code == 403


async def test_a_load_test_through_the_api(client):
    agents = (await client.get("/load/agents", headers=auth(ADMIN_TOKEN))).json()
    assert agents["configured"] and agents["mock_available"]
    assert agents["agents"][0]["reachable"] and agents["agents"][0]["cpus"] == 2

    r = await client.post("/load/runs", json=BODY, headers=auth(ADMIN_TOKEN))
    assert r.status_code == 202, r.text
    run_id = r.json()["id"]
    assert "points" not in r.json()
    # One test at a time, of either kind.
    busy = await client.post("/runs", json={"mode": "mock"}, headers=auth(ADMIN_TOKEN))
    assert busy.status_code == 409 and "load test" in busy.json()["detail"]
    await client.app.state.loads.wait()

    run = (await client.get(f"/load/runs/{run_id}", headers=auth(ADMIN_TOKEN))).json()
    assert run["status"] == "finished" and len(run["points"]) == 2
    listed = (await client.get("/load/runs", headers=auth(ADMIN_TOKEN))).json()
    assert listed["items"][0]["id"] == run_id and "points" not in listed["items"][0]
    events = await client.get(f"/load/runs/{run_id}/events", headers=auth(ADMIN_TOKEN))
    assert "event: snapshot" in events.text and "event: end" in events.text
    assert (await client.post(f"/load/runs/{run_id}/cancel", headers=auth(ADMIN_TOKEN))).status_code == 409


async def test_requests_that_cannot_run_are_refused(client):
    r = await client.post("/load/runs", json={**BODY, "agents": ["nobody"]}, headers=auth(ADMIN_TOKEN))
    assert r.status_code == 422 and "unknown agents" in r.json()["detail"]
    external = {
        **BODY,
        "target": "external",
        "external": {"secret": "k-1234567890", "model": "m", "base_url": "http://u:p@x"},
    }
    r = await client.post("/load/runs", json=external, headers=auth(ADMIN_TOKEN))
    assert r.status_code == 422
    assert "k-1234567890" not in r.text, "a refused request does not echo the key"
