"""The client modules against a fake pacod-id (httpx.MockTransport) with real RS256 keys."""

# No `from __future__ import annotations`: FastAPI reads the dependencies' annotations at
# definition time, and they name local variables.

import asyncio
import contextlib
import json
import time
from typing import Annotated, Any

import httpx
import jwt
import pytest
from cryptography.hazmat.primitives.asymmetric import rsa
from fastapi import Depends, FastAPI
from fastapi.testclient import TestClient

from pacod_id import grant as grant_module
from pacod_id.authz import Catalog, Manifest
from pacod_id.cache import AuthzCache
from pacod_id.client import IdClient, IdConfig
from pacod_id.fastapi import IdDependencies, Principal
from pacod_id.grant import DelegatedGrant, SignedOut
from pacod_id.verify import InvalidToken, JwtVerifier

ISSUER = "http://id.test"
CONFIG = IdConfig(
    issuers=(ISSUER,),
    internal_url="http://id-internal.test",
    client_id="pacod-gw-tester",
    client_secret="a-secret-for-the-tester-0123456789",
    product="gw",
    audience="pacod-gw",
)


class FakeId:
    """Enough of pacod-id's internal listener for the client modules."""

    def __init__(self) -> None:
        self.key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
        self.kid = "k1"
        self.refreshes = 0
        self.revoked: set[str] = set()
        self.changes: list[Any] = []
        self.snapshots = 0
        self.head = 0
        self.jwks_hangs = False
        self.services = 0
        self.refused: set[str] = set()
        self.snapshot_body: dict[str, Any] = {
            "seq": 5,
            "bindings": [],
            "disabled": [],
            "deleted": [],
            "revoked_sids": [],
        }

    def token(self, **over: Any) -> str:
        now = int(time.time())
        claims = {
            "iss": ISSUER,
            "sub": "usr_1",
            "aud": "pacod-gw",
            "iat": now,
            "exp": now + 300,
            "pid_kind": "user",
            "sid": "sid_1",
            "client_id": "console",
            "scope": "openid",
        }
        claims.update(over.pop("claims", {}))
        typ = over.pop("typ", "at+jwt")
        kid = over.pop("kid", self.kid)
        return jwt.encode(claims, self.key, algorithm="RS256", headers={"typ": typ, "kid": kid})

    def jwks(self) -> dict[str, Any]:
        jwk = json.loads(jwt.algorithms.RSAAlgorithm.to_jwk(self.key.public_key()))
        jwk.update({"kid": self.kid, "alg": "RS256", "use": "sig"})
        return {"keys": [jwk]}

    async def handle(self, request: httpx.Request) -> httpx.Response:
        path = request.url.path
        form = dict(httpx.QueryParams(request.content.decode())) if request.method == "POST" else {}
        if path == "/oidc/jwks":
            if self.jwks_hangs:
                await asyncio.sleep(60)
            return httpx.Response(200, json=self.jwks())
        if path.startswith("/internal/") and request.headers.get("authorization", "")[7:] in self.refused:
            refused = {"type": "unauthorized", "details": {"code": "invalid_token"}}
            return httpx.Response(401, json={"error": refused})
        if path == "/oidc/token":
            grant = form.get("grant_type")
            if grant == "client_credentials":
                self.services += 1
                return httpx.Response(200, json={"access_token": f"svc{self.services}", "expires_in": 300})
            if grant == "refresh_token":
                if form["refresh_token"] in self.revoked:
                    return httpx.Response(400, json={"error": "invalid_grant"})
                self.refreshes += 1
                await asyncio.sleep(0.02)
                return httpx.Response(
                    200,
                    json={
                        "access_token": f"at{self.refreshes}",
                        "refresh_token": f"rt{self.refreshes}",
                        "expires_in": 300,
                    },
                )
            if form.get("requested_token_type", "").endswith("refresh_token"):
                return httpx.Response(
                    200, json={"access_token": "at0", "refresh_token": "rt0", "expires_in": 300}
                )
        if path == "/oidc/revoke":
            self.revoked.add(form["token"])
            return httpx.Response(200)
        if path == "/internal/v1/authz/snapshot":
            self.snapshots += 1
            # A snapshot is never older than the changes already served.
            return httpx.Response(
                200, json={**self.snapshot_body, "seq": max(self.snapshot_body["seq"], self.head)}
            )
        if path == "/internal/v1/authz/changes":
            if self.changes:
                answer = self.changes.pop(0)
                if answer is None:
                    return httpx.Response(200, json=None)
                if isinstance(answer, int):
                    return httpx.Response(
                        answer, json={"error": {"type": "gone", "details": {"code": "cursor_expired"}}}
                    )
                self.head = max(self.head, answer["seq"])
                # The server's state moves with its changes: a later snapshot has them.
                for e in answer["events"]:
                    if e["kind"] == "binding.added":
                        self.snapshot_body["bindings"].append(
                            {k: e[k] for k in ("id", "principal", "role", "path")}
                        )
                    if e["kind"] == "session.revoked":
                        self.snapshot_body["revoked_sids"].append({"sid": e["sid"], "sub": e["sub"]})
                return httpx.Response(200, json=answer)
            await asyncio.sleep(0.05)
            after = int(request.url.params["after"])
            return httpx.Response(200, json={"seq": after, "events": []})
        return httpx.Response(404, json={"error": {"type": "not_found"}})


@pytest.fixture
def fake() -> FakeId:
    return FakeId()


@pytest.fixture
async def client(fake: FakeId):
    c = IdClient(CONFIG, httpx.AsyncClient(transport=httpx.MockTransport(fake.handle)))
    yield c
    await c.aclose()


async def test_a_delegated_grant_refreshes_once_and_ends(fake: FakeId, client: IdClient, monkeypatch) -> None:
    g = await DelegatedGrant.start(client, "the-admins-access-token")
    assert await g.access_token() == "at0"
    # Time passes: due, and five callers at once refresh it once.
    later = time.monotonic() + 290
    monkeypatch.setattr(grant_module, "_now", lambda: later)
    tokens = await asyncio.gather(*(g.access_token() for _ in range(5)))
    assert tokens == ["at1"] * 5 and fake.refreshes == 1
    await g.close()
    assert "rt1" in fake.revoked
    with pytest.raises(SignedOut):
        await g.access_token()


async def test_a_refused_refresh_is_a_sign_out(fake: FakeId, client: IdClient, monkeypatch) -> None:
    g = await DelegatedGrant.start(client, "t")
    fake.revoked.add("rt0")
    later = time.monotonic() + 290
    monkeypatch.setattr(grant_module, "_now", lambda: later)
    with pytest.raises(SignedOut):
        await g.access_token()


async def test_only_access_tokens_for_the_product_count(fake: FakeId, client: IdClient) -> None:
    v = JwtVerifier(client)
    t = await v.verify(fake.token())
    assert (t.sub, t.kind, t.sid, t.client_id) == ("usr_1", "user", "sid_1", "console")
    for bad in [
        fake.token(typ="JWT"),
        fake.token(claims={"aud": "pacod-id"}),
        fake.token(claims={"iss": "http://elsewhere.test"}),
        fake.token(claims={"exp": int(time.time()) - 120}),
        fake.token(claims={"sid": None}),
        fake.token(kid="unknown"),
        "not.a.token",
    ]:
        with pytest.raises(InvalidToken):
            await v.verify(bad)


async def test_the_copy_follows_the_feed_and_reloads_after_a_gap(fake: FakeId, client: IdClient) -> None:
    fake.snapshot_body["bindings"] = [
        {"id": "rb_1", "principal": "usr_1", "role": "gw.viewer", "path": ["platform"]}
    ]
    fake.changes = [
        {
            "seq": 7,
            "events": [
                {
                    "seq": 6,
                    "kind": "binding.added",
                    "id": "rb_2",
                    "principal": "usr_1",
                    "role": "gw.auditor",
                    "path": ["platform"],
                },
                {"seq": 7, "kind": "session.revoked", "sid": "sid_9", "sub": "usr_1"},
            ],
        },
        {"seq": 9, "events": []},
        410,
    ]
    cache = AuthzCache(client)
    seen: list[str] = []
    cache.on_event(lambda e: seen.append(e["kind"]))
    assert not cache.ready and cache.feed_age is None
    cache.start()
    assert await cache.wait_for(9, 2)
    assert cache.ready and cache.feed_age is not None
    assert {b.role for b in cache.bindings_of("usr_1")} == {"gw.viewer", "gw.auditor"}
    assert cache.sid_revoked("sid_9") and seen == ["binding.added", "session.revoked"]
    # The 410 makes it read the snapshot again.
    for _ in range(40):
        if fake.snapshots >= 2:
            break
        await asyncio.sleep(0.05)
    assert fake.snapshots == 2
    await cache.stop()


GW = """
product: gw
scope_types:
  workspace: { parent: organisation }
roles:
  platform_admin: { at: [platform], permissions: ["*"], protected: true }
  viewer: { at: [platform, organisation, workspace], permissions: [org.read] }
grant:
  default: members.manage
"""


def test_fastapi_dependencies(fake: FakeId) -> None:
    http = httpx.AsyncClient(transport=httpx.MockTransport(fake.handle))
    client = IdClient(CONFIG, http)
    cache = AuthzCache(client)
    ids = IdDependencies(JwtVerifier(client), cache, Catalog([Manifest.from_yaml(GW)]))
    fake.snapshot_body["bindings"] = [
        {"id": "rb_1", "principal": "usr_1", "role": "gw.viewer", "path": ["platform"]}
    ]
    reads = ids.require_permission("gw.org.read", lambda request: ("platform", "organisation:o1"))
    manages = ids.require_permission("gw.org.manage", lambda request: ("platform", "organisation:o1"))

    @contextlib.asynccontextmanager
    async def lifespan(_: FastAPI):
        cache.start()
        await cache.wait_for(5, 2)
        yield
        await cache.stop()

    app = FastAPI(lifespan=lifespan)

    @app.get("/read")
    async def read(p: Annotated[Principal, Depends(reads)]) -> dict[str, str]:
        return {"sub": p.token.sub}

    @app.get("/manage")
    async def manage(p: Annotated[Principal, Depends(manages)]) -> dict[str, str]:
        return {"sub": p.token.sub}

    with TestClient(app) as tc:
        good = {"authorization": f"Bearer {fake.token()}"}
        assert tc.get("/read", headers=good).json() == {"sub": "usr_1"}
        assert tc.get("/manage", headers=good).status_code == 403
        assert tc.get("/read").status_code == 401
        id_token = {"authorization": f"Bearer {fake.token(typ='JWT')}"}
        assert tc.get("/read", headers=id_token).status_code == 401


async def test_a_silent_pacod_id_holds_up_no_request(fake: FakeId, client: IdClient) -> None:
    v = JwtVerifier(client, refresh_every=0)
    token = fake.token()
    await v.verify(token)
    fake.jwks_hangs = True
    started = time.monotonic()
    await asyncio.gather(*(v.verify(token) for _ in range(20)))
    assert time.monotonic() - started < 0.5, "verified with the known keys, the read in the background"


async def test_the_follower_survives_odd_answers_and_tells_pending_deletions(
    fake: FakeId, client: IdClient
) -> None:
    fake.snapshot_body["deleted"] = ["usr_gone"]
    fake.changes = [None, {"seq": 6, "events": []}]
    cache = AuthzCache(client)
    seen: list[tuple[str, str]] = []
    cache.on_event(lambda e: seen.append((e["kind"], e.get("sub", ""))))
    cache.start()
    assert await cache.wait_for(6, 4), "still following after an answer of another shape"
    assert ("principal.deleted", "usr_gone") in seen, "a deletion pending at load is told"
    await cache.stop()


async def test_a_refused_service_token_is_replaced_once(fake: FakeId, client: IdClient) -> None:
    await client.snapshot()
    fake.refused.add("svc1")
    await client.snapshot()
    assert fake.services == 2, "a new token after the refusal"
