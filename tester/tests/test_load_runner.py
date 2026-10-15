"""Load tests against the fake core and fake agents: the scene, the agents' parts, the points,
cancelling, the caps, and what never gets stored."""

import asyncio
import dataclasses
import json
from pathlib import Path

import pytest

from pgw_tester.catalog import Catalog
from pgw_tester.engine import Runner
from pgw_tester.guard import Busy, Exclusive
from pgw_tester.load import runner as load_runner
from pgw_tester.load.agents import AgentRef
from pgw_tester.load.model import LoadRequest, LoadStore
from pgw_tester.load.runner import AgentsBusy, AgentsUnavailable, Invalid, LoadRunner
from pgw_tester.results import RunRequest, Starter
from pgw_tester.store import Store

from .fakeagent import LOAD_TOKEN, FakeAgent, transport
from .fakecore import ADMIN_TOKEN, FakeCore

STARTER = Starter(id="usr_1", display="admin@example.com")
STAGES = [{"duration_s": 0, "target": 4}, {"duration_s": 3, "target": 4}]


@pytest.fixture(autouse=True)
def quick(monkeypatch):
    """Seconds in the runner become milliseconds here."""
    monkeypatch.setattr(load_runner, "POLL_EVERY", 0.01)
    monkeypatch.setattr(load_runner, "SAMPLE_EVERY", 0.01)
    monkeypatch.setattr(load_runner, "STALE_AFTER", 0.05)
    monkeypatch.setattr(load_runner, "LOST_AFTER", 3)


@pytest.fixture
def agents() -> list[FakeAgent]:
    return [FakeAgent("a"), FakeAgent("b")]


def make(settings, core: FakeCore, agents: list[FakeAgent], exclusive: Exclusive | None = None) -> LoadRunner:
    s = dataclasses.replace(
        settings,
        load_agents=tuple(AgentRef(a.name, f"http://{a.host}") for a in agents),
        load_token=LOAD_TOKEN,
        load_mock_url="http://load-mock",
    )
    return LoadRunner(s, LoadStore(s.data_dir / "load"), exclusive or Exclusive(), transport(core, agents))


def request(**over) -> LoadRequest:
    data = {"target": "mock", "mock": {}, "load": {"stages": STAGES}, "agents": ["a", "b"], **over}
    return LoadRequest.model_validate(data)


async def run(loads: LoadRunner, req: LoadRequest):
    started = await loads.start(STARTER, ADMIN_TOKEN, req)
    await loads.wait()
    stored = loads.store.load(started.id)
    assert stored is not None
    return stored


def admin_writes(core: FakeCore) -> list[tuple[str, str]]:
    return [(m, p) for m, p in core.calls if p.startswith("/api/v1") and m != "GET"]


async def test_a_mock_load_test_sets_up_splits_merges_and_cleans_up(settings, core, agents):
    loads = make(settings, core, agents)
    done = await run(loads, request())
    assert done.status == "finished", done.error
    a, b = agents
    # The scene: a load mock with the profile in its URLs, a key for this mode and model only.
    provider = next(o for p, o in core.objects.items() if o.get("name") == "tester-load-mock-openai")
    assert provider["base_url"] == "http://load-mock/x-mock/delay=200,chunk_delay=20,chunks=20/v1"
    assert provider["proxy_base_url"] == "http://load-mock/x-mock/delay=200,chunk_delay=20,chunks=20"
    model = next(o for o in core.objects.values() if o.get("name") == "tester-load-mock/gpt-mock")
    assert model["free"] is True
    key_body = next(json.loads(b) for p, b in _posts(core) if p.endswith("/keys"))
    assert key_body["allowed_modes"] == ["managed"] and key_body["allowed_models"] == [model["id"]]
    assert key_body["expires_at"] and "max_concurrency" not in key_body
    # One part each, of the same plan, with the scratch key.
    assert [p["part"] for p in a.prepared + b.prepared] == [0, 1]
    assert {p["parts"] for p in a.prepared + b.prepared} == {2}
    assert a.prepared[0]["key"].startswith("pgw_") and "base_url" not in a.prepared[0]
    assert [s["target"] for s in a.prepared[0]["profile"]["stages"]] == [2, 2]
    assert a.prepared[0]["profile"]["warmup_connections"] == 2
    assert a.started == b.started == [done.id]
    # Three seconds from each agent, added up.
    assert [p.t for p in done.points] == [0, 1, 2]
    assert all(p.agents == 2 and p.completed == 20 and p.ok == 18 for p in done.points)
    assert done.points[0].status == {"200": 18, "503": 2}
    assert done.points[0].ttfb is not None and done.points[0].ttfb.p50 == pytest.approx(200, rel=0.01)
    assert done.points[0].cpu_max == 25.0, "50% of one CPU on an agent with two"
    assert done.points[-1].gw_in_flight == 7
    s = done.summary
    assert s is not None
    assert (s.calls, s.ok, s.failed, s.status["503"]) == (60, 54, 6, 6)
    assert s.gw_delay is not None and s.gw_delay.p50 == pytest.approx(1.5, rel=0.01)
    assert any("503" in w for w in done.warnings)
    # The key goes first, then everything else; nothing is left over.
    deleted = [p for p in core.deleted]
    assert "/keys/" in deleted[0]
    assert any(p.startswith("/orgs/") and p.count("/") == 2 for p in deleted)
    assert done.scene.leftovers == []
    assert loads.active is None and loads.exclusive.holder is None

    # The next run writes its own profile into the same provider.
    agents[0].next_t = agents[1].next_t = 0
    again = await run(loads, request(mock={"delay_ms": 50, "chunks": 3}))
    assert again.status == "finished"
    put = [body for path, body in core.puts if path.startswith("/providers/")][-1]
    assert put["base_url"] == "http://load-mock/x-mock/delay=50,chunk_delay=20,chunks=3/v1"


def _posts(core: FakeCore) -> list[tuple[str, bytes]]:
    return core.posted


async def test_a_cancel_stops_the_agents_and_still_cleans_up(settings, core, agents):
    for a in agents:
        a.endless = True
    loads = make(settings, core, agents)
    started = await loads.start(STARTER, ADMIN_TOKEN, request())
    for _ in range(200):
        if loads.active and loads.active.run.points:
            break
        await asyncio.sleep(0.01)
    assert loads.cancel(started.id)
    await loads.wait()
    done = loads.store.load(started.id)
    assert done is not None and done.status == "cancelled"
    assert all(a.cancelled == [started.id] for a in agents)
    assert done.scene.leftovers == [] and "/keys/" in core.deleted[0]


async def test_live_load_tests_are_capped_and_acknowledged(settings, core, agents):
    core.objects["/models/mdl1"] = {
        "id": "mdl1",
        "name": "acme/gpt",
        "upstream_id": "gpt",
        "provider_id": "prv1",
        "state": "active",
    }
    core.objects["/providers/prv1"] = {"id": "prv1", "name": "openai", "family": "openai"}
    loads = make(settings, core, agents)
    live = {"target": "live", "mock": None, "live": {"model_id": "mdl1"}}
    with pytest.raises(Invalid, match="cost money"):
        await loads.start(STARTER, ADMIN_TOKEN, request(**live, load={"stages": STAGES, "max_calls": 100}))
    with pytest.raises(Invalid, match="at most 1000"):
        await loads.start(
            STARTER,
            ADMIN_TOKEN,
            request(**live, acknowledge_cost=True, load={"stages": STAGES, "max_calls": 5000}),
        )
    with pytest.raises(Invalid, match="number of calls"):
        await loads.start(STARTER, ADMIN_TOKEN, request(**live, acknowledge_cost=True))
    assert loads.exclusive.holder is None, "a refused start holds nothing"

    done = await run(loads, request(**live, acknowledge_cost=True, load={"stages": STAGES, "max_calls": 100}))
    assert done.status == "finished", done.error
    limit = next(json.loads(b) for p, b in _posts(core) if p.endswith("/limits"))
    assert (limit["metric"], limit["window"], limit["amount"]) == ("requests", "lifetime", 100.0)
    # Two probes and the sanity call come out of the 100.
    assert sum(p["profile"]["max_calls"] for p in agents[0].prepared + agents[1].prepared) == 97
    key_body = next(json.loads(b) for p, b in _posts(core) if p.endswith("/keys"))
    assert key_body["max_concurrency"] == 4
    assert agents[0].prepared[0]["request"]["headers"]["x-gw-retries"] == "1"


async def test_an_own_key_is_used_and_never_stored(settings, core, agents):
    loads = make(settings, core, agents)
    secret = "pgw_mine_ownsecretownsecret123"
    done = await run(
        loads,
        request(
            target="own",
            mock=None,
            own={"secret": secret, "model": "acme/gpt"},
            acknowledge_cost=True,
            load={"stages": STAGES, "max_calls": 500},
        ),
    )
    assert done.status == "finished", done.error
    assert admin_writes(core) == [], "nothing is set up for an own key"
    assert all(p["key"] == secret for p in agents[0].prepared + agents[1].prepared)
    stored = (loads.store.dir / f"{done.id}.json").read_text()
    assert "ownsecret" not in stored
    assert done.request["own"]["key_hint"] == "pgw_mine_…"
    assert done.points[-1].gw_in_flight == 7, "this gateway's own figures are read"


async def test_an_external_gateway_gets_the_url_and_nothing_else(settings, core, agents):
    loads = make(settings, core, agents)
    secret = "sk-external-gateway-key-123456"
    started = await loads.start(
        STARTER,
        ADMIN_TOKEN,
        request(
            target="external",
            mock=None,
            external={"secret": secret, "model": "openai/gpt-dev", "base_url": "http://lite.internal:14080/"},
            acknowledge_cost=True,
            load={"stages": STAGES, "max_calls": 100_000},
        ),
    )
    queue = loads.subscribe(started.id)
    assert queue is not None
    await loads.wait()
    events = []
    while not queue.empty():
        events.append(queue.get_nowait())
    done = loads.store.load(started.id)
    assert done is not None and done.status == "finished", done and done.error
    assert done.base_url == "http://lite.internal:14080"
    assert all(p["base_url"] == "http://lite.internal:14080" for p in agents[0].prepared + agents[1].prepared)
    assert [c for c in core.calls if c[1].startswith("/api/v1")] == [], "no management API at all"
    assert all(p.gw_in_flight is None for p in done.points)
    text = (loads.store.dir / f"{done.id}.json").read_text() + json.dumps(events, default=str)
    assert secret not in text
    assert done.request["external"]["key_hint"].startswith("sha256:")


async def test_a_refused_probe_ends_the_run_before_any_load(settings, core, agents):
    agents[1].probe = {
        "status": 401,
        "ttfb_ms": 2,
        "total_ms": 2,
        "error": None,
        "body_excerpt": "invalid key",
    }
    agents[1].probe_state = "failed"
    loads = make(settings, core, agents)
    done = await run(loads, request())
    assert done.status == "error"
    assert "agent b" in done.error and "401" in done.error
    assert agents[0].started == [] and agents[0].cancelled == [done.id]
    assert done.scene.leftovers == []


async def test_an_agent_that_stops_answering_is_left_behind(settings, core, agents):
    agents[0].seconds = 5
    agents[1].answers = 1
    loads = make(settings, core, agents)
    done = await run(loads, request())
    assert done.status == "finished"
    assert any("agent b stopped answering" in w for w in done.warnings)
    assert [p.agents for p in done.points][-1] == 1


async def test_agents_must_be_there_free_and_able(settings, core, agents):
    loads = make(settings, core, agents)
    agents[1].unreachable = True
    with pytest.raises(AgentsUnavailable, match="agent b"):
        await loads.start(STARTER, ADMIN_TOKEN, request())
    agents[1].unreachable = False
    agents[1].busy = {"run_id": "20261015-000000-aaaaaa", "state": "running"}
    with pytest.raises(AgentsBusy, match="busy"):
        await loads.start(STARTER, ADMIN_TOKEN, request())
    agents[1].busy = None
    agents[1].caps = {**agents[1].caps, "max_concurrency": 1}
    with pytest.raises(Invalid, match="at a time"):
        await loads.start(STARTER, ADMIN_TOKEN, request())
    with pytest.raises(Invalid, match="unknown agents"):
        await loads.start(STARTER, ADMIN_TOKEN, request(agents=["z"]))
    view = await loads.agents_view()
    assert [v["reachable"] for v in view] == [True, True]
    assert loads.exclusive.holder is None


async def test_functional_runs_and_load_tests_exclude_each_other(settings, core, agents, tmp_path: Path):
    for a in agents:
        a.endless = True
    shared = Exclusive()
    loads = make(settings, core, agents, shared)
    runner = Runner(settings, Catalog(tmp_path / "cases"), Store(tmp_path / "runs"), core.transport(), shared)
    started = await loads.start(STARTER, ADMIN_TOKEN, request())
    with pytest.raises(Busy, match="load test"):
        await runner.start(STARTER, ADMIN_TOKEN, RunRequest(mode="mock"))
    loads.cancel(started.id)
    await loads.wait()
    core.slow["/healthz"] = 0.5
    run = await runner.start(STARTER, ADMIN_TOKEN, RunRequest(mode="mock", cases=["data-plane-live"]))
    with pytest.raises(Busy, match=f"run {run.id}"):
        await loads.start(STARTER, ADMIN_TOKEN, request())
    runner.cancel(run.id)
    await runner.wait()
    assert shared.holder is None


def test_runs_that_were_going_are_interrupted_with_their_leftovers(tmp_path: Path):
    from pgw_tester.load.model import LoadRun

    store = LoadStore(tmp_path)
    run = LoadRun(id="20261015-101500-a1b2c3", target="mock", started_by=STARTER, status="running")
    run.scene.ledger = ["/orgs/o1", "/orgs/o1/workspaces/w1", "/orgs/o1/workspaces/w1/keys/k1"]
    store.save(run)
    store.recover()
    again = store.load(run.id)
    assert again is not None and again.status == "interrupted"
    assert again.scene.leftovers == list(reversed(run.scene.ledger))
