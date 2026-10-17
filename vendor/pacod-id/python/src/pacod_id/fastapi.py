"""FastAPI dependencies: who is calling, and whether they may.

    ids = IdDependencies(verifier, cache, catalog)

    admin = ids.require_permission("shop.admin.read", lambda request: ("platform",))

    @app.get("/admin/things")
    async def things(p: Principal = Depends(admin)): ...

Requires the ``fastapi`` extra.
"""

from __future__ import annotations

from collections.abc import Callable, Sequence
from dataclasses import dataclass

from fastapi import HTTPException, Request

from .authz import Catalog, Subject
from .cache import AuthzCache
from .verify import AccessToken, InvalidToken, JwtVerifier, KeysUnavailable


@dataclass(frozen=True)
class Principal:
    token: AccessToken
    subject: Subject


class IdDependencies:
    def __init__(
        self, verifier: JwtVerifier, cache: AuthzCache, catalog: Catalog, max_staleness: float = 60
    ) -> None:
        self.verifier = verifier
        self.cache = cache
        self.catalog = catalog
        self.max_staleness = max_staleness

    async def require_principal(self, request: Request) -> Principal:
        """A checked caller: the token, the session not ended, the account not disabled."""
        auth = request.headers.get("authorization", "")
        if not auth.lower().startswith("bearer "):
            raise HTTPException(401, "a bearer token is needed", headers={"www-authenticate": "Bearer"})
        try:
            token = await self.verifier.verify(auth[7:].strip())
        except InvalidToken as e:
            raise HTTPException(
                401, "the token is not valid here", headers={"www-authenticate": "Bearer"}
            ) from e
        except KeysUnavailable as e:
            raise HTTPException(503, "identity_unavailable") from e
        if token.sid and self.cache.sid_revoked(token.sid):
            raise HTTPException(401, "the session has ended", headers={"www-authenticate": "Bearer"})
        if self.cache.is_disabled(token.sub):
            raise HTTPException(403, "the account is disabled")
        kind = "user" if token.kind == "user" else "service"
        return Principal(token, Subject(kind, self.cache.bindings_of(token.sub)))

    def require_permission(
        self, permission: str, path: Callable[[Request], Sequence[str]]
    ) -> Callable[[Request], object]:
        """A dependency that also needs ``permission`` at the scope ``path(request)`` gives; refused
        with 503 while the copy of the bindings is older than ``max_staleness`` seconds."""

        async def dependency(request: Request) -> Principal:
            age = self.cache.feed_age
            if age is None or age > self.max_staleness:
                raise HTTPException(503, "identity_unavailable")
            p = await self.require_principal(request)
            if not self.catalog.allowed(p.subject, permission, tuple(path(request))):
                raise HTTPException(403, "you may not do this")
            return p

        return dependency
