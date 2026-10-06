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
#: family: (provider name, base URL below the mock's origin, model name, upstream id, preset)
MOCK = {
    "openai": ("tester-mock-openai", "/v1", "tester-mock/gpt-mock", "gpt-mock", "openai-chat"),
    "anthropic": ("tester-mock-anthropic", "", "tester-mock/claude-mock", "claude-mock", "anthropic-budget"),
}
FAMILIES = ("openai", "anthropic")


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

    async def items(self, name: str, path: str) -> list[dict[str, Any]]:
        """Every item of a collection, following its cursor."""
        out: list[dict[str, Any]] = []
        cursor = None
        for _ in range(50):
            local: dict[str, Any] = {}
            query = {"limit": 200, **({"cursor": cursor} if cursor else {})}
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
    providers = {p.get("name"): p for p in await rec.items("find the mock providers", "/providers")}
    models = {m.get("name"): m for m in await rec.items("find the mock models", "/models")}
    out: dict[str, dict[str, str]] = {}
    for family, (pname, suffix, mname, upstream, preset) in MOCK.items():
        local: dict[str, Any] = {}
        provider = providers.get(pname)
        if provider is None:
            await rec(
                {
                    "name": f"register provider {pname}",
                    "mgmt": {
                        "method": "POST",
                        "path": "/providers",
                        "body": {"name": pname, "family": family, "base_url": mock + suffix},
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
    """The existing models the admin chose, checked to be active and of the right family."""
    out: dict[str, dict[str, str]] = {}
    for family in FAMILIES:
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
                "name": f"check the provider of the {family} model",
                "mgmt": {"path": "/providers/{{pid}}"},
                "expect": {"json": {"family": family}},
                "save": {"pname": "json.name"},
            },
            local,
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
