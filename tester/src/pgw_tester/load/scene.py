"""What a load test sets up on this gateway, and takes down after it.

Mock targets use three providers and models of their own (`tester-load-mock-*`), kept between
runs as the functional tests keep theirs: the gateway only deletes models that were never
active. Each run writes its latency profile into the provider's URLs
(`…/x-mock/delay=200,chunk_delay=20,chunks=20/v1`): managed mode forwards no `x-mock` header,
but calls every path below the base URL, and the mock reads options from there. The
`tester-load-mock-*` objects are for load tests only, so the next run simply writes its own.

Every run gets a scratch organisation, workspace and key, deleted afterwards (the key first, so
that it cannot be used once the run is over). The key works for the run's mode and model only,
and expires by itself shortly after the run should have ended.
"""

from datetime import timedelta
from typing import Any

from .. import fixtures
from ..fixtures import Recorder
from ..results import StepResult, now
from ..steps import Context
from .model import LoadRequest

LOAD_MOCK_SECRET = "tester-load-mock-provider-secret"
#: family: (provider name, base URL below the mock's origin, proxy URL below the mock's origin,
#: model name, upstream id, capability preset, what the provider is besides its name and URLs)
LOAD_MOCK: dict[str, tuple[str, str, str, str, str, str, dict[str, str]]] = {
    "openai": (
        "tester-load-mock-openai",
        "/v1",
        "",
        "tester-load-mock/gpt-mock",
        "gpt-mock",
        "openai-chat",
        {"family": "openai"},
    ),
    "anthropic": (
        "tester-load-mock-anthropic",
        "",
        "",
        "tester-load-mock/claude-mock",
        "claude-mock",
        "anthropic-budget",
        {"family": "anthropic"},
    ),
    "gemini": (
        "tester-load-mock-gemini",
        "/emulate/gemini/v1beta",
        "/emulate/gemini",
        "tester-load-mock/gemini-mock",
        "gemini-mock",
        "gemini-3-flash",
        {"family": "gemini", "managed_api": "native"},
    ),
}
#: The embedding model, at the OpenAI mock: (model name, upstream id, capability preset, more).
LOAD_EMBED = ("tester-load-mock/embed-mock", "text-embedding-3-small", "embedding", {"kind": "embedding"})
#: How long the scratch key outlives the run's planned end.
KEY_GRACE = timedelta(minutes=15)


async def ensure_load_mock(rec: Recorder, family: str, profile: str, embeddings: bool) -> dict[str, str]:
    """The load mock of a family, with the run's profile in its URLs; returns its model."""
    mock = rec.ctx.settings.load_mock_url
    rec.ctx.redact.add(LOAD_MOCK_SECRET)
    pname, suffix, proxy_suffix, mname, upstream, preset, kind = LOAD_MOCK[family]
    more: dict[str, Any] = {"free": True}
    if embeddings:
        mname, upstream, preset, extra = LOAD_EMBED
        more.update(extra)
    base_url = f"{mock}/x-mock/{profile}{suffix}"
    proxy_url = f"{mock}/x-mock/{profile}{proxy_suffix}"
    providers = {
        p.get("name"): p for p in await rec.items("find the load mocks", "/providers", "tester-load-mock")
    }
    models = {m.get("name"): m for m in await rec.items("find their models", "/models", "tester-load-mock")}
    local: dict[str, Any] = {}
    provider = providers.get(pname)
    if provider is None:
        await rec(
            {
                "name": f"register provider {pname}",
                "mgmt": {
                    "method": "POST",
                    "path": "/providers",
                    "body": {"name": pname, "base_url": base_url, "proxy_base_url": proxy_url, **kind},
                },
                "expect": {"status": 201},
                "save": {"pid": "json.id"},
                "cleanup": False,
            },
            local,
        )
        await rec(
            {
                "name": f"give {pname} a credential",
                "mgmt": {
                    "method": "POST",
                    "path": "/providers/{{pid}}/credentials",
                    "body": {"label": "tester", "secret": LOAD_MOCK_SECRET},
                },
                "expect": {"status": 201},
                "cleanup": False,
            },
            local,
        )
    else:
        local["pid"] = provider["id"]
        await rec(
            {"name": f"read {pname}", "mgmt": {"path": "/providers/{{pid}}"}, "save": {"pdoc": "json"}},
            local,
        )
        doc = dict(local["pdoc"])
        doc.update({"base_url": base_url, "proxy_base_url": proxy_url})
        await rec(
            {
                "name": f"set the latency profile of {pname}",
                "mgmt": {"method": "PUT", "path": "/providers/{{pid}}", "body": doc},
                "expect": {"status": 200},
            },
            local,
        )
    model = models.get(mname)
    if model is None:
        await rec(
            {
                "name": f"register model {mname}",
                "mgmt": {
                    "method": "POST",
                    "path": "/models",
                    "body": {
                        "name": mname,
                        "provider_id": local["pid"],
                        "upstream_id": upstream,
                        "capability_preset": preset,
                        **more,
                    },
                },
                "expect": {"status": 201},
                "save": {"mid": "json.id"},
                "cleanup": False,
            },
            local,
        )
    else:
        local["mid"] = model["id"]
    if model is None or model.get("state") != "active":
        await rec(
            {
                "name": f"activate {mname}",
                "mgmt": {"method": "POST", "path": "/models/{{mid}}/transition", "body": {"to": "active"}},
            },
            local,
        )
    return {"id": local["mid"], "name": mname, "provider": pname, "upstream": upstream}


async def live_model(rec: Recorder, model_id: str) -> dict[str, str]:
    """An existing model the admin chose: it must be active."""
    local: dict[str, Any] = {"mid": model_id}
    await rec(
        {
            "name": "look up the model",
            "mgmt": {"path": "/models/{{mid}}"},
            "expect": {"json": {"state": "active"}},
            "save": {"name": "json.name", "upstream": "json.upstream_id", "pid": "json.provider_id"},
        },
        local,
    )
    await rec(
        {
            "name": "look up its provider",
            "mgmt": {"path": "/providers/{{pid}}"},
            "save": {"pname": "json.name", "pdoc": "json"},
        },
        local,
    )
    doc = local.get("pdoc") or {}
    return {
        "id": model_id,
        "name": local["name"],
        "provider": local["pname"],
        "upstream": local["upstream"],
        "family": doc.get("family", ""),
        "managed_api": doc.get("managed_api", "openai_compatible"),
    }


async def scratch_scene(
    rec: Recorder,
    name: str,
    req: LoadRequest,
    model: dict[str, str],
    peak_in_flight: int,
    budget_limit: int | None,
) -> None:
    """The organisation, its workspace, the model granted, the key, and in live mode a limit
    that stops the run's calls at `budget_limit` should anything else fail to."""
    await rec(
        {
            "name": "create the scratch organisation",
            "mgmt": {"method": "POST", "path": "/orgs", "body": {"name": name}},
            "expect": {"status": 201},
            "save": {"org": "json.id"},
        }
    )
    await rec(
        {
            "name": "create its workspace",
            "mgmt": {
                "method": "POST",
                "path": "/orgs/{{org}}/workspaces",
                # Audit on request: a load test's calls are not recorded in full.
                "body": {"name": "main", "settings": {"audit": {"mode": "on_request"}}},
            },
            "expect": {"status": 201},
            "save": {"ws": "json.id"},
        }
    )
    await fixtures.grant(rec, "load", model)
    expires = now() + timedelta(seconds=req.load.duration_s()) + KEY_GRACE
    key: dict[str, Any] = {
        "name": f"tester-load-{name[-6:]}",
        "allowed_modes": [req.mode],
        "allowed_models": [model["id"]],
        "expires_at": expires.isoformat(),
    }
    if req.target == "live":
        key["max_concurrency"] = max(1, peak_in_flight)
    await rec(
        {
            "name": "create the run's key",
            "mgmt": {"method": "POST", "path": "/orgs/{{org}}/workspaces/{{ws}}/keys", "body": key},
            "expect": {"status": 201},
            "save": {"key": "json.secret", "key_id": "json.id"},
        }
    )
    if budget_limit is not None:
        await rec(
            {
                "name": f"stop the workspace at {budget_limit} calls",
                "mgmt": {
                    "method": "POST",
                    "path": "/orgs/{{org}}/workspaces/{{ws}}/limits",
                    "body": {
                        "name": "tester-load-calls",
                        "subject": "workspace",
                        "metric": "requests",
                        "window": "lifetime",
                        "amount": float(budget_limit),
                        "enforcement": "request",
                    },
                },
                "expect": {"status": 201},
            }
        )


async def sanity_call(
    ctx: Context, results: list[StepResult], path: str, headers: dict[str, str], body: str
) -> None:
    """One call from the tester before any load, to see that the scene works (an open circuit
    breaker, a model the gateway cannot reach) before the agents are asked."""
    rec = Recorder(ctx, results)
    call_headers = {k: v for k, v in headers.items() if k != "content-type"}
    await rec(
        {
            "name": "one call before the load",
            "call": {
                "method": "POST",
                "path": path,
                "headers": call_headers,
                "body": body,
                "key": "{{key}}",
                "timeout": 120,
            },
        }
    )


async def teardown(ctx: Context, results: list[StepResult], ledger: list[str]) -> list[str]:
    """Deletes what the run made, the key first; returns what could not be deleted."""
    keys = [p for p in ledger if "/keys/" in p]
    order = keys + [p for p in reversed(ledger) if p not in keys]
    leftovers: list[str] = []
    for path in order:
        result = await fixtures.delete_path(ctx, path)
        results.append(result)
        if result.status != "passed":
            leftovers.append(path)
    return leftovers
