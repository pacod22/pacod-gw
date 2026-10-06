"""Who may use the tester: platform admins, proven by their console session.

The console forwards the admin's session token. The tester does not trust the network: it asks
the core who the token belongs to, and serves platform admins only. The answer is cached for a
short while, keyed by a hash of the token; the token itself is not kept.
"""

import hashlib
import time
from dataclasses import dataclass

import httpx
from fastapi import HTTPException, Request

CACHE_SECONDS = 30.0
_CACHE_LIMIT = 256


@dataclass(frozen=True)
class Admin:
    id: str
    display: str
    token: str


class SessionCheck:
    def __init__(self, admin_url: str, transport: httpx.AsyncBaseTransport | None = None):
        self.admin_url = admin_url
        self.transport = transport
        self._cache: dict[str, tuple[float, str, str]] = {}

    async def __call__(self, request: Request) -> Admin:
        header = request.headers.get("authorization", "")
        scheme, _, token = header.partition(" ")
        token = token.strip()
        if scheme.lower() != "bearer" or not token:
            raise HTTPException(401, "Sign in to the console first.")
        digest = hashlib.sha256(token.encode()).hexdigest()
        hit = self._cache.get(digest)
        if hit and hit[0] > time.monotonic():
            return Admin(id=hit[1], display=hit[2], token=token)

        try:
            async with httpx.AsyncClient(transport=self.transport, timeout=10.0) as http:
                r = await http.get(
                    f"{self.admin_url}/api/v1/session", headers={"authorization": f"Bearer {token}"}
                )
        except httpx.HTTPError:
            raise HTTPException(502, "The gateway's management API does not answer.") from None
        if r.status_code == 401:
            raise HTTPException(401, "The session has ended; sign in again.")
        if r.status_code != 200:
            raise HTTPException(502, f"The gateway answered {r.status_code} to the session check.")
        principal = r.json()
        if principal.get("platform_admin") is not True:
            raise HTTPException(403, "Testing is for platform admins only.")
        if len(self._cache) >= _CACHE_LIMIT:
            self._cache.clear()
        self._cache[digest] = (
            time.monotonic() + CACHE_SECONDS,
            principal["id"],
            principal.get("display", ""),
        )
        return Admin(id=principal["id"], display=principal.get("display", ""), token=token)
