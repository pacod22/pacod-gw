"""What a `scratch` suite gets before its cases: an organisation, workspace and key of its own,
and the models of the run's mode granted to it.

Mock mode keeps one pair of providers and models (`tester-mock-*`) between runs: the gateway
only deletes models that were never active, and a provider only when no model uses it, so
fresh ones per run would pile up. Everything below the scratch organisation is removed after
each suite.
"""

import json
from typing import Any

from .model import Step
from .results import StepResult
from .steps import Context, run_step

MOCK_SECRET = "tester-mock-provider-secret"
#: slot: (provider name, base URL below the mock's origin, model name, upstream id, preset,
#: what the provider is besides its name and base URL)
MOCK = {
    "openai": (
        "tester-mock-openai",
        "/v1",
        "tester-mock/gpt-mock",
        "gpt-mock",
        "openai-chat",
        {"family": "openai"},
    ),
    "anthropic": (
        "tester-mock-anthropic",
        "",
        "tester-mock/claude-mock",
        "claude-mock",
        "anthropic-budget",
        {"family": "anthropic"},
    ),
    # The mock's emulation of Gemini 3's own API, which refuses a tool turn without its signature.
    "gemini": (
        "tester-mock-gemini",
        "/emulate/gemini/v1beta",
        "tester-mock/gemini-mock",
        "gemini-mock",
        "gemini-3-flash",
        {"family": "gemini", "managed_api": "native"},
    ),
}
SLOTS = ("openai", "anthropic", "gemini")

#: Models of mock mode only, by the name cases use them under (`{{model.embedding}}`): (provider
#: name, the provider preset its provider is made from or None for the OpenAI mock's, base URL
#: below the mock's origin, model name, upstream id, capability preset, more of the model).
MOCK_EXTRA = {
    "embedding": (
        "tester-mock-openai",
        None,
        "/v1",
        "tester-mock/embed-mock",
        "text-embedding-3-small",
        "embedding",
        {"kind": "embedding"},
    ),
    "xai": (
        "tester-mock-xai",
        "xai",
        "/emulate/xai/v1",
        "tester-mock/grok-mock",
        "grok-4",
        "xai-reasoning",
        {},
    ),
    # Vertex AI's express mode: an API key, so no token to mint at Google.
    "vertex": (
        "tester-mock-vertex",
        "vertex-express",
        "/emulate/vertex/v1/publishers/google",
        "tester-mock/vertex-mock",
        "gemini-3.8-flash",
        "gemini-3-flash",
        {},
    ),
}


def fits(slot: str, family: str, managed_api: str) -> bool:
    """Whether a provider of this family and managed API serves the slot: the `openai` slot is
    every provider managed mode speaks OpenAI's dialect to."""
    if slot == "openai":
        return family in ("openai", "ollama") or (family == "gemini" and managed_api != "native")
    if slot == "gemini":
        return family == "gemini" and managed_api == "native"
    return family == slot


class FixtureFailed(Exception):
    pass


class Recorder:
    """Runs fixture steps and keeps their results with the suite's setup."""

    def __init__(self, ctx: Context, results: list[StepResult]):
        self.ctx = ctx
        self.results = results

    async def __call__(self, spec: dict[str, Any], variables: dict[str, Any] | None = None) -> dict[str, Any]:
        step = Step.model_validate(spec)
        scope = self.ctx.variables if variables is None else variables
        result = await run_step(self.ctx, step, scope)
        self.results.append(result)
        if result.status != "passed":
            raise FixtureFailed(f"{result.name}: {'; '.join(result.failures) or result.detail}")
        return scope

    async def items(self, name: str, path: str, search: str = "") -> list[dict[str, Any]]:
        """Every item of a collection (whose name contains `search`), following its cursor."""
        out: list[dict[str, Any]] = []
        cursor = None
        for _ in range(50):
            local: dict[str, Any] = {}
            query = {
                "limit": 200,
                **({"q": search} if search else {}),
                **({"cursor": cursor} if cursor else {}),
            }
            await self(
                {"name": name, "mgmt": {"path": path, "query": query}, "save": {"page": "json.items"}},
                local,
            )
            # The cursor is optional: read it from the step's own answer.
            out += local.get("page") or []
            cursor = _cursor(self.results[-1])
            if not cursor:
                break
        return out


def _cursor(result: StepResult) -> str | None:
    try:
        body = json.loads(result.response.body) if result.response else {}
    except ValueError:
        return None
    return body.get("next_cursor") if isinstance(body, dict) else None


async def scratch(rec: Recorder, name: str) -> None:
    """The organisation, its workspace and a key."""
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
                # Audit on request: a call is recorded in full only when it asks for it.
                "body": {
                    "name": "main",
                    "tag_keys": ["purpose"],
                    "settings": {"audit": {"mode": "on_request"}},
                },
            },
            "expect": {"status": 201},
            "save": {"ws": "json.id"},
        }
    )
    await rec(
        {
            "name": "create an API key",
            "mgmt": {
                "method": "POST",
                "path": "/orgs/{{org}}/workspaces/{{ws}}/keys",
                "body": {"name": "tester"},
            },
            "expect": {"status": 201},
            "save": {"key": "json.secret", "key_id": "json.id"},
        }
    )


async def mock_models(rec: Recorder) -> dict[str, dict[str, str]]:
    """Makes sure the mock providers and their active models exist; returns them by family."""
    mock = rec.ctx.settings.mock_url
    rec.ctx.redact.add(MOCK_SECRET)
    # Found by name: an installation that has been running a while has more than a page of them.
    providers = {
        p.get("name"): p for p in await rec.items("find the mock providers", "/providers", "tester-mock")
    }
    models = {m.get("name"): m for m in await rec.items("find the mock models", "/models", "tester-mock")}
    presets: dict[str, dict[str, Any]] = {}
    if any(p not in providers for p, preset, *_ in MOCK_EXTRA.values() if preset):
        local: dict[str, Any] = {}
        await rec(
            {
                "name": "read the provider presets",
                "mgmt": {"path": "/provider-presets"},
                "save": {"list": "json"},
            },
            local,
        )
        presets = {p.get("name"): p for p in local.get("list") or []}
    wanted = [
        (family, pname, suffix, mname, upstream, preset, kind, {})
        for family, (pname, suffix, mname, upstream, preset, kind) in MOCK.items()
    ]
    for name, (pname, pset, suffix, mname, upstream, preset, more) in MOCK_EXTRA.items():
        p = presets.get(pset or "", {})
        kind = {
            k: p[k] for k in ("family", "managed_api", "platform", "auth", "quirks") if p.get(k) is not None
        }
        wanted.append((name, pname, suffix, mname, upstream, preset, kind or {"family": "openai"}, more))
    out: dict[str, dict[str, str]] = {}
    for family, pname, suffix, mname, upstream, preset, kind, more in wanted:
        local = {}
        provider = providers.get(pname)
        if provider is None:
            await rec(
                {
                    "name": f"register provider {pname}",
                    "mgmt": {
                        "method": "POST",
                        "path": "/providers",
                        "body": {"name": pname, "base_url": mock + suffix, **kind},
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
                        "body": {"label": "tester", "secret": MOCK_SECRET},
                    },
                    "expect": {"status": 201},
                    "cleanup": False,
                },
                local,
            )
            provider = {"id": local["pid"], "name": pname}
            providers[pname] = provider
        model = models.get(mname)
        local["mid"] = model["id"] if model else None
        if model is None:
            await rec(
                {
                    "name": f"register model {mname}",
                    "mgmt": {
                        "method": "POST",
                        "path": "/models",
                        "body": {
                            "name": mname,
                            "provider_id": provider["id"],
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
        if model is None or model.get("state") == "draft":
            await rec(
                {
                    "name": f"price {mname}",
                    "mgmt": {
                        "method": "PUT",
                        "path": "/models/{{mid}}/price",
                        "body": {"currency": "USD", "input": 2.0, "output": 10.0},
                    },
                    "expect": {"status": [200, 201]},
                },
                local,
            )
        if model is None or model.get("state") != "active":
            await rec(
                {
                    "name": f"activate {mname}",
                    "mgmt": {
                        "method": "POST",
                        "path": "/models/{{mid}}/transition",
                        "body": {"to": "active"},
                    },
                },
                local,
            )
        out[family] = {"id": local["mid"], "name": mname, "provider": pname, "upstream": upstream}
    return out


async def live_models(rec: Recorder, chosen: dict[str, str | None]) -> dict[str, dict[str, str]]:
    """The existing models the admin chose, checked to be active and to fit their slot."""
    out: dict[str, dict[str, str]] = {}
    for family in SLOTS:
        model_id = chosen.get(family)
        if not model_id:
            continue
        local: dict[str, Any] = {"mid": model_id}
        await rec(
            {
                "name": f"look up the {family} model",
                "mgmt": {"path": "/models/{{mid}}"},
                "expect": {"json": {"state": "active"}},
                "save": {"name": "json.name", "upstream": "json.upstream_id", "pid": "json.provider_id"},
            },
            local,
        )
        await rec(
            {
                "name": f"look up the provider of the {family} model",
                "mgmt": {"path": "/providers/{{pid}}"},
                "save": {"pname": "json.name", "pdoc": "json"},
            },
            local,
        )
        doc = local.get("pdoc") or {}
        pfamily, papi = doc.get("family", ""), doc.get("managed_api", "openai_compatible")
        if not fits(family, pfamily, papi):
            raise FixtureFailed(
                f"the {family} model's provider is of the family {pfamily!r} ({papi}), "
                "which does not serve that slot"
            )
        out[family] = {
            "id": model_id,
            "name": local["name"],
            "provider": local["pname"],
            "upstream": local["upstream"],
        }
    return out


async def grant(rec: Recorder, family: str, model: dict[str, str]) -> None:
    await rec(
        {
            "name": f"grant {model['name']}",
            "mgmt": {"method": "POST", "path": "/orgs/{{org}}/grants", "body": {"model_id": model["id"]}},
            "expect": {"status": 201},
        }
    )
    rec.ctx.variables.setdefault("model", {})[family] = model["name"]
    rec.ctx.variables.setdefault("model_id", {})[family] = model["id"]
    rec.ctx.variables.setdefault("provider", {})[family] = model["provider"]
    rec.ctx.variables.setdefault("upstream", {})[family] = model["upstream"]
    rec.ctx.available.add(family)
