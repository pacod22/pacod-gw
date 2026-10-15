"""A fake of the core's management API and data plane, as far as the tests need it."""

import asyncio
import itertools
import json

import httpx

ADMIN_TOKEN = "pgws_admin_0123456789abcdef"
VIEWER_TOKEN = "pgws_viewer_0123456789abcdef"


class FakeCore:
    def __init__(self):
        self.objects: dict[str, dict] = {}
        self.deleted: list[str] = []
        self.calls: list[tuple[str, str]] = []
        self.revoked: set[str] = set()
        self.keys: dict[str, str] = {}
        self._ids = itertools.count(1)
        #: Says which DELETEs fail, to test leftovers.
        self.undeletable = lambda path: False
        #: Data plane paths that answer only after this many seconds, or fail to connect.
        self.slow: dict[str, float] = {}
        self.broken: set[str] = set()
        #: The answer of the playground preview: (status, body).
        self.preview: tuple[int, dict] = (
            200,
            {"route": "same_dialect", "model_served": "acme/gpt", "body": {"model": "gpt"}},
        )
        self.previewed: list[dict] = []
        #: pacod-id's token endpoint (host id-internal): delegated grants and their tokens.
        self.grant_life = 300
        self.signed_out = False
        #: Refreshes allowed before the session "ends" (None: no limit).
        self.refresh_limit: int | None = None
        self.grant_tokens: set[str] = set()
        self.exchanged: list[str] = []
        self.refreshed = 0
        self.revoked_grants: list[str] = []
        #: The bearer of every call to the management API.
        self.bearers: list[str] = []
        #: Every POST to the management API: (path, raw body).
        self.posted: list[tuple[str, bytes]] = []
        #: Every PUT to the management API: (path, body).
        self.puts: list[tuple[str, dict]] = []
        #: What GET /monitoring/live answers: one instance, up to date.
        self.live: dict = {
            "config_version": 1,
            "instances": [{"id": "gw-1", "applied_version": 1}],
            "traffic": {"in_flight": 7, "requests_per_second": 3.5, "rejected_per_second": 0.0},
            "spool_bytes": 0,
            "events_dropped": 0,
        }
        #: Answers of the data plane by path: (status, headers, body).
        self.data: dict[str, tuple[int, dict, object]] = {
            "/healthz": (200, {}, {"status": "ok"}),
            "/v1/chat/completions": (
                200,
                {"x-gw-request-id": "gw_1", "x-gw-cost": "0.01"},
                {"object": "chat.completion", "usage": {"cost": {"amount": 0.01}}},
            ),
        }

    def transport(self) -> httpx.MockTransport:
        return httpx.MockTransport(self.handle)

    async def handle(self, request: httpx.Request) -> httpx.Response:
        path = request.url.path
        self.calls.append((request.method, path))
        if request.url.host == "core-admin":
            return self.admin(request, path)
        if request.url.host == "id-internal":
            return self.identity(request, path)
        if path in self.broken:
            raise httpx.ConnectError("connection refused", request=request)
        if path in self.slow:
            await asyncio.sleep(self.slow[path])
        return self.dataplane(request, path)

    def identity(self, request: httpx.Request, path: str) -> httpx.Response:
        form = dict(httpx.QueryParams(request.content.decode()))
        if path == "/oidc/revoke":
            self.revoked_grants.append(form["token"])
            return httpx.Response(200)
        refreshing = form.get("grant_type") == "refresh_token"
        if refreshing and self.refresh_limit is not None and self.refreshed >= self.refresh_limit:
            self.signed_out = True
        if self.signed_out:
            return httpx.Response(400, json={"error": "invalid_grant"})
        if form.get("grant_type") == "refresh_token":
            self.refreshed += 1
        else:
            self.exchanged.append(form["subject_token"])
        n = self.refreshed
        self.grant_tokens.add(f"grant-at-{n}")
        return httpx.Response(
            200,
            json={
                "access_token": f"grant-at-{n}",
                "refresh_token": f"grant-rt-{n}",
                "expires_in": self.grant_life,
            },
        )

    def admin(self, request: httpx.Request, path: str) -> httpx.Response:
        auth = request.headers.get("authorization", "")
        if path != "/api/v1/session":
            self.bearers.append(auth.removeprefix("Bearer "))
        if path == "/api/v1/session":
            if auth == f"Bearer {ADMIN_TOKEN}":
                return httpx.Response(
                    200, json={"id": "usr_1", "display": "admin@example.com", "platform_admin": True}
                )
            if auth == f"Bearer {VIEWER_TOKEN}":
                return httpx.Response(200, json={"id": "usr_2", "display": "viewer", "platform_admin": False})
            return httpx.Response(401, json={"title": "unauthorized"})
        if auth != f"Bearer {ADMIN_TOKEN}" and auth.removeprefix("Bearer ") not in self.grant_tokens:
            return httpx.Response(401, json={"title": "unauthorized"})
        api = path.removeprefix("/api/v1")
        if request.method == "POST":
            self.posted.append((api, request.content))
            if api.endswith("/playground/transform"):
                self.previewed.append(json.loads(request.content))
                return httpx.Response(self.preview[0], json=self.preview[1])
            if api.endswith("/revoke"):
                self.revoked.add(api.split("/")[-2])
                return httpx.Response(200, json={})
            if api.endswith("/transition"):
                return httpx.Response(200, json={"state": "active"})
            body = json.loads(request.content or b"{}")
            obj_id = f"id{next(self._ids)}"
            obj = {"id": obj_id, **body}
            # Like the core, never send a credential's secret back.
            obj.pop("secret", None)
            if api.endswith("/keys"):
                obj["secret"] = f"pgw_{obj_id}_secretsecretsecret{obj_id}"
                self.keys[obj["secret"]] = obj_id
            self.objects[f"{api}/{obj_id}"] = obj
            return httpx.Response(201, json=obj)
        if request.method == "DELETE":
            if self.undeletable(api):
                return httpx.Response(409, json={"title": "conflict"})
            self.deleted.append(api)
            self.objects.pop(api, None)
            return httpx.Response(204)
        if request.method == "PUT":
            self.puts.append((api, json.loads(request.content or b"{}")))
            if api in self.objects:
                # Like the core: the object as written, with its next version.
                obj = {**json.loads(request.content or b"{}"), "id": self.objects[api]["id"]}
                obj["version"] = int(self.objects[api].get("version", 1)) + 1
                self.objects[api] = obj
                return httpx.Response(200, json=obj)
            return httpx.Response(201, json={})
        if api == "/monitoring/live":
            return httpx.Response(200, json=self.live)
        if api == "/provider-presets":
            # The presets the tester's mock-only providers are made from.
            return httpx.Response(
                200,
                json=[
                    {"name": "xai", "family": "openai", "managed_api": "openai_compatible", "quirks": {}},
                    {
                        "name": "vertex-express",
                        "family": "gemini",
                        "managed_api": "native",
                        "platform": "vertex",
                        "auth": "x_goog_api_key",
                        "quirks": {},
                    },
                ],
            )
        if api in ("/providers", "/models"):
            items = [o for p, o in self.objects.items() if p.rsplit("/", 1)[0] == api]
            return httpx.Response(200, json={"items": items, "next_cursor": None})
        if api in self.objects:
            return httpx.Response(200, json=self.objects[api])
        return httpx.Response(404, json={"title": "not found"})

    def dataplane(self, request: httpx.Request, path: str) -> httpx.Response:
        auth = request.headers.get("authorization", "").removeprefix("Bearer ") or request.headers.get(
            "x-api-key", ""
        )
        if path != "/healthz":
            key_id = self.keys.get(auth)
            if key_id is None or key_id in self.revoked:
                return httpx.Response(401, json={"error": "invalid key"})
        status, headers, body = self.data.get(path, (404, {}, {"error": "no route"}))
        if isinstance(body, str):
            return httpx.Response(status, headers=headers, text=body)
        return httpx.Response(status, headers=headers, json=body)
