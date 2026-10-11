"""Runs against the fake core: fixtures, cases, teardown, cancelling, one run at a time."""

import asyncio
from pathlib import Path

import pytest

from pgw_tester.catalog import Catalog
from pgw_tester.engine import Busy, Invalid, Runner
from pgw_tester.results import RunParams, RunRequest, Starter
from pgw_tester.store import Store

from .fakecore import ADMIN_TOKEN, FakeCore

FLOW = """
suite: flow
title: Flow
setup:
  - mgmt: { method: POST, path: "/orgs/{{org}}/workspaces/{{ws}}/templates", body: { name: t } }
    expect: { status: 201 }
cases:
  - id: flow-call
    title: a call with cost
    steps:
      - call: { method: POST, path: /v1/chat/completions, body: { model: "{{model.openai}}" } }
        expect:
          headers: { x-gw-cost: { exists: true } }
          json: { usage.cost.amount: { type: number } }
  - id: flow-fail
    title: a call that fails
    steps:
      - call: { path: /missing }
  - id: flow-revoke
    title: a revoked key
    steps:
      - mgmt: { method: POST, path: "/orgs/{{org}}/workspaces/{{ws}}/keys", body: { name: k } }
        expect: { status: 201 }
        save: { spare: json.secret, spare_id: json.id }
      - mgmt: { method: POST, path: "/orgs/{{org}}/workspaces/{{ws}}/keys/{{spare_id}}/revoke" }
      - call: { method: POST, path: /v1/chat/completions, key: "{{spare}}" }
        expect: { status: 401 }
        retry: { within: 1s, every: 10ms }
  - id: flow-live-only
    title: live only
    modes: [live]
    steps: [{ sleep: 0 }]
  - id: flow-mock-control
    title: needs a scriptable mock
    needs: [mock_control]
    steps: [{ sleep: 0 }]
"""

SLOW = """
suite: slow
title: Slow
cases:
  - id: slow-one
    title: waits
    steps: [{ sleep: 5 }]
  - id: slow-two
    title: waits too
    steps: [{ sleep: 5 }]
"""

ADMIN = Starter(id="usr_1", display="admin@example.com")


def runner_for(settings, core: FakeCore, *suites: str) -> Runner:
    catalog = Catalog(settings.data_dir / "cases")
    for text in suites:
        catalog.upload(text)
    return Runner(settings, catalog, Store(settings.data_dir / "runs"), core.transport())


def org_paths(core: FakeCore) -> list[str]:
    return [p for p in core.deleted if p.startswith("/orgs/") and p.count("/") == 2]


async def test_a_mock_run_passes_fails_skips_and_cleans_up(settings, core):
    runner = runner_for(settings, core, FLOW)
    flow_ids = ["flow-call", "flow-fail", "flow-revoke", "flow-live-only", "flow-mock-control"]
    run = await runner.start(ADMIN, ADMIN_TOKEN, RunRequest(mode="mock", cases=flow_ids))
    await runner.wait()

    run = runner.store.load(run.id)
    assert run is not None
    [flow_suite] = run.suites
    statuses = {c.id: c.status for c in flow_suite.cases}
    assert statuses == {
        "flow-call": "passed",
        "flow-fail": "failed",
        "flow-revoke": "passed",
        "flow-live-only": "skipped",
        "flow-mock-control": "passed",
    }
    assert run.status == "failed"
    assert run.counts.failed >= 1

    # Everything below the scratch organisation is deleted, the organisation last; the mock
    # providers and models stay for the next run.
    assert flow_suite.leftovers == []
    assert core.deleted[-1] == org_paths(core)[-1]
    assert not any(p.startswith(("/providers", "/models")) for p in core.deleted)
    assert any("/keys/" in p for p in core.deleted)

    # Secrets never reach the stored run.
    stored = (settings.data_dir / "runs" / f"{run.id}.json").read_text()
    assert ADMIN_TOKEN not in stored
    assert "secretsecretsecret" not in stored
    assert "tester-mock-provider-secret" not in stored


async def test_the_mock_models_are_made_once_and_reused(settings, core):
    runner = runner_for(settings, core)
    await runner.start(ADMIN, ADMIN_TOKEN, RunRequest(mode="mock", cases=["proxy-openai"]))
    await runner.wait()
    made = [c for c in core.calls if c == ("POST", "/api/v1/models")]
    await runner.start(ADMIN, ADMIN_TOKEN, RunRequest(mode="mock", cases=["proxy-openai"]))
    await runner.wait()
    assert [c for c in core.calls if c == ("POST", "/api/v1/models")] == made
    assert len(made) == 6, (
        "one model per slot (OpenAI's dialect, Anthropic, Gemini's own API) and the mock-only ones "
        "(an embedding model, xAI, Vertex AI)"
    )


async def test_unmet_needs_skip_cases(settings, core):
    settings = type(settings)(**{**settings.__dict__, "mock_control_url": ""})
    runner = runner_for(settings, core, FLOW)
    run = await runner.start(ADMIN, ADMIN_TOKEN, RunRequest(mode="mock", cases=["flow-mock-control"]))
    await runner.wait()
    case = runner.store.load(run.id).suites[0].cases[0]
    assert case.status == "skipped"
    assert "mock provider" in case.reason


async def test_a_failed_setup_errors_the_cases_and_still_tears_down(settings, core):
    broken = FLOW.replace("expect: { status: 201 }\ncases", "expect: { status: 299 }\ncases", 1)
    runner = runner_for(settings, core, broken)
    run = await runner.start(ADMIN, ADMIN_TOKEN, RunRequest(mode="mock", cases=["flow-call"]))
    await runner.wait()
    suite = runner.store.load(run.id).suites[0]
    assert suite.cases[0].status == "error"
    assert "setup failed" in suite.cases[0].reason
    assert org_paths(core), "the organisation was deleted"


async def test_failed_deletes_are_reported_as_leftovers(settings, core):
    core.undeletable = lambda path: path.startswith("/orgs/") and path.count("/") == 2
    runner = runner_for(settings, core, FLOW)
    run = await runner.start(ADMIN, ADMIN_TOKEN, RunRequest(mode="mock", cases=["flow-call"]))
    await runner.wait()
    run = runner.store.load(run.id)
    assert len(run.suites[0].leftovers) == 1
    assert run.summary()["leftovers"] == 1


async def test_stop_on_fail_skips_the_rest_and_still_cleans_up(settings, core):
    runner = runner_for(settings, core, FLOW)
    cases = ["flow-call", "flow-fail", "flow-revoke"]
    run = await runner.start(ADMIN, ADMIN_TOKEN, RunRequest(mode="mock", cases=cases, stop_on_fail=True))
    await runner.wait()
    suite = runner.store.load(run.id).suites[0]
    assert [c.status for c in suite.cases] == ["passed", "failed", "skipped"]
    assert suite.cases[2].reason == "stopped after the first failure"
    assert org_paths(core), "the organisation was deleted"


async def test_keep_resources_skips_teardown(settings, core):
    runner = runner_for(settings, core, FLOW)
    run = await runner.start(
        ADMIN, ADMIN_TOKEN, RunRequest(mode="mock", cases=["flow-call"], keep_resources=True)
    )
    await runner.wait()
    assert core.deleted == []
    assert runner.store.load(run.id).suites[0].leftovers


async def test_one_run_at_a_time_and_cancel(settings, core):
    runner = runner_for(settings, core, SLOW)
    run = await runner.start(ADMIN, ADMIN_TOKEN, RunRequest(mode="mock", cases=["slow-one", "slow-two"]))
    with pytest.raises(Busy):
        await runner.start(ADMIN, ADMIN_TOKEN, RunRequest(mode="mock", cases=["slow-one"]))
    await asyncio.sleep(0.1)
    assert runner.cancel(run.id)
    await asyncio.wait_for(runner.wait(), 2)
    run = runner.store.load(run.id)
    assert run.status == "cancelled"
    assert [c.status for c in run.suites[0].cases] == ["skipped", "skipped"]
    assert runner.active is None
    assert org_paths(core), "cancelling still tears down"


async def test_live_mode_uses_the_chosen_models(settings, core):
    core.objects["/models/m1"] = {
        "id": "m1",
        "name": "acme/gpt",
        "state": "active",
        "upstream_id": "gpt",
        "provider_id": "p1",
    }
    core.objects["/providers/p1"] = {"id": "p1", "name": "acme", "family": "openai"}
    runner = runner_for(settings, core, FLOW)
    run = await runner.start(
        ADMIN, ADMIN_TOKEN, RunRequest(mode="live", cases=["flow-call"], params=RunParams(openai_model="m1"))
    )
    await runner.wait()
    run = runner.store.load(run.id)
    assert run.suites[0].cases[0].status == "passed"
    assert ("POST", "/api/v1/providers") not in core.calls


async def test_a_model_that_does_not_fit_its_slot_is_refused(settings, core):
    core.objects["/models/m1"] = {
        "id": "m1",
        "name": "acme/gpt",
        "state": "active",
        "upstream_id": "gpt",
        "provider_id": "p1",
    }
    core.objects["/providers/p1"] = {"id": "p1", "name": "acme", "family": "openai"}
    runner = runner_for(settings, core, FLOW)
    run = await runner.start(
        ADMIN, ADMIN_TOKEN, RunRequest(mode="live", cases=["flow-call"], params=RunParams(gemini_model="m1"))
    )
    await runner.wait()
    run = runner.store.load(run.id)
    case = run.suites[0].cases[0]
    assert case.status != "passed"
    assert "does not serve that slot" in case.reason


async def test_requests_that_cannot_run_are_refused(settings, core):
    runner = runner_for(settings, core)
    with pytest.raises(Invalid, match="at least one model"):
        await runner.start(ADMIN, ADMIN_TOKEN, RunRequest(mode="live"))
    with pytest.raises(Invalid, match="unknown cases"):
        await runner.start(ADMIN, ADMIN_TOKEN, RunRequest(mode="mock", cases=["nope"]))
    no_mock = type(settings)(**{**settings.__dict__, "mock_url": "", "mock_control_url": ""})
    with pytest.raises(Invalid, match="mock provider"):
        await runner_for(no_mock, core).start(ADMIN, ADMIN_TOKEN, RunRequest(mode="mock"))


def test_interrupted_runs_are_marked(tmp_path: Path):
    from pgw_tester.results import Run

    store = Store(tmp_path)
    store.save(Run(id="20260101-000000-abcdef", mode="mock", started_by=ADMIN, status="running"))
    store.recover()
    assert store.load("20260101-000000-abcdef").status == "interrupted"
    assert store.load("../etc/passwd") is None
