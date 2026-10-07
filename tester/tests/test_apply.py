"""Waiting for a configuration change to reach every gateway instance."""

import asyncio
import time

import httpx

from pgw_tester.config import Settings
from pgw_tester.redact import Redactor
from pgw_tester.steps import Context


def context(settings: Settings, handler) -> Context:
    http = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    return Context(settings=settings, http=http, redact=Redactor(), token="t", cancel=asyncio.Event())


async def test_waits_until_every_instance_has_the_version(settings: Settings) -> None:
    asked = 0

    def handler(request: httpx.Request) -> httpx.Response:
        nonlocal asked
        asked += 1
        assert request.url.path == "/api/v1/monitoring/live"
        assert request.headers["authorization"] == "Bearer t"
        behind = 41 if asked < 3 else 42
        return httpx.Response(
            200,
            json={
                "config_version": 42,
                "instances": [
                    {"id": "core", "applied_version": 42},
                    {"id": "gw-1", "applied_version": behind},
                ],
            },
        )

    await context(settings, handler).await_applied()
    assert asked == 3


async def test_gives_up_when_the_core_does_not_say(settings: Settings) -> None:
    started = time.monotonic()
    await context(settings, lambda _: httpx.Response(404, json={})).await_applied()
    await context(settings, lambda _: httpx.Response(200, json={"config_version": 1})).await_applied()
    # An instance that never catches up costs the limit, not more.
    lagging = {"config_version": 2, "instances": [{"id": "gw-1", "applied_version": 1}]}
    await context(settings, lambda _: httpx.Response(200, json=lagging)).await_applied(limit=0.3)
    assert time.monotonic() - started < 2
