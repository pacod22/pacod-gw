"""A delegated grant: a long task acting for a person, for example a test run of the
gateway's tester started by an admin. It is bound to the admin's session: signing out ends it.
"""

from __future__ import annotations

import asyncio
import contextlib
import time

from .client import ApiError, IdClient, Tokens

#: Refresh this long before the access token expires.
REFRESH_BEFORE = 30


def _now() -> float:
    return time.monotonic()


class SignedOut(Exception):
    """pacod-id refused the grant: the person signed out, or it was revoked or expired."""


class DelegatedGrant:
    """Access tokens for a person's long task, refreshed in time; :meth:`close` revokes it."""

    def __init__(self, client: IdClient, tokens: Tokens) -> None:
        self._client = client
        self._tokens = tokens
        #: When the current tokens came, by the same clock the refresh is timed with.
        self._at = _now()
        self._lock = asyncio.Lock()
        self._closed = False

    @classmethod
    async def start(cls, client: IdClient, subject_token: str) -> DelegatedGrant:
        """From the person's access token for this product (forwarded by the product's BFF)."""
        try:
            tokens = await client.delegated_grant(subject_token)
        except ApiError as e:
            # Only the person's session can be over; a refused client (a wrong secret, a client
            # that may not exchange) is the service's own mistake and says so.
            if e.code == "invalid_grant":
                raise SignedOut(e.code) from e
            raise
        if not tokens.refresh_token:
            raise ApiError(200, "no_refresh_token", "pacod-id gave no delegated grant")
        return cls(client, tokens)

    def _due(self) -> bool:
        life = self._tokens.expires_in or 300
        return _now() >= self._at + life - REFRESH_BEFORE

    async def access_token(self) -> str:
        """A valid access token; one refresh at a time when due."""
        if self._closed:
            raise SignedOut("closed")
        if not self._due():
            return self._tokens.access_token
        async with self._lock:
            if self._due():
                try:
                    self._tokens = await self._client.refresh(self._tokens.refresh_token or "")
                    self._at = _now()
                except ApiError as e:
                    if e.code == "invalid_grant":
                        raise SignedOut(e.code) from e
                    raise
        return self._tokens.access_token

    async def close(self) -> None:
        """Revokes the grant (best effort); later calls raise :class:`SignedOut`."""
        # Under the lock: a refresh under way finishes first, and the token revoked is the
        # newest, not the one it replaced (which pacod-id would no longer know).
        async with self._lock:
            if self._closed:
                return
            self._closed = True
            if self._tokens.refresh_token:
                with contextlib.suppress(ApiError):
                    await self._client.revoke(self._tokens.refresh_token)
