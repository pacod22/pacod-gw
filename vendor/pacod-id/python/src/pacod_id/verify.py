"""Checks of the access tokens a product's Python API receives: pacod-id's
signature (keys from the internal listener), ``typ`` at+jwt, one of the product's issuers, its
audience, and the time with 30 seconds of leeway. id_tokens, logout tokens and tokens for
pacod-id's own API are refused.
"""

from __future__ import annotations

import asyncio
import contextlib
import time
from dataclasses import dataclass

import jwt
from jwt import PyJWK

from .client import IdClient

LEEWAY = 30
#: Keys are read for an unknown key id at most this often, whether the last read worked or not.
REFETCH_AT_MOST = 30.0
#: Known keys are read again this often, in the background: no request waits for it.
REFRESH_EVERY = 3600.0
#: A read of the keys that takes longer is given up.
FETCH_TIMEOUT = 5.0


class InvalidToken(Exception):
    """Not a token of pacod-id for this product (or expired)."""


class KeysUnavailable(Exception):
    """pacod-id's keys cannot be read: answer 503, not 401."""


@dataclass(frozen=True)
class AccessToken:
    sub: str
    kind: str
    sid: str | None
    client_id: str
    scope: str
    issuer: str
    actor: str | None
    expires_at: int


class JwtVerifier:
    def __init__(self, client: IdClient, refresh_every: float = REFRESH_EVERY) -> None:
        self._client = client
        self._keys: dict[str, PyJWK] = {}
        self._read_at: float | None = None
        #: When a read last started, successful or not.
        self._tried_at: float | None = None
        self._refresh_every = refresh_every
        self._lock = asyncio.Lock()
        self._background: asyncio.Task[None] | None = None

    def _claim_read(self) -> bool:
        """The right to start a read, at most once per REFETCH_AT_MOST."""
        now = time.monotonic()
        if self._tried_at is not None and now - self._tried_at < REFETCH_AT_MOST:
            return False
        self._tried_at = now
        return True

    async def _read(self) -> None:
        async with self._lock:
            try:
                r = await self._client.http.get(self._client.url("/oidc/jwks"), timeout=FETCH_TIMEOUT)
                r.raise_for_status()
                keys = {}
                for k in r.json().get("keys", []):
                    if (
                        k.get("use", "sig") == "sig"
                        and k.get("kid")
                        and k.get("alg") in ("RS256", "ES256", None)
                    ):
                        try:
                            keys[k["kid"]] = PyJWK(k)
                        except jwt.PyJWKError:
                            continue
            except Exception as e:  # any failure to read is the same to the caller
                raise KeysUnavailable(str(e)) from e
            self._keys = keys
            self._read_at = time.monotonic()

    async def refresh_keys(self) -> None:
        """Reads the keys now (also done by itself when an unknown key id shows up)."""
        self._tried_at = time.monotonic()
        await self._read()

    async def _refresh_in_background(self) -> None:
        # On failure the known keys stay; the next due request tries again.
        with contextlib.suppress(KeysUnavailable):
            await self._read()

    async def _key(self, kid: str) -> PyJWK:
        key = self._keys.get(kid)
        if key is not None:
            # Known: answer with it, and read newer keys in the background when due. A pacod-id
            # that does not answer never holds up a request here.
            due = self._read_at is not None and time.monotonic() - self._read_at >= self._refresh_every
            if due and self._claim_read():
                self._background = asyncio.get_running_loop().create_task(self._refresh_in_background())
            return key
        # An unknown key id (pacod-id rotated, or nothing read yet): one read at a time, at most
        # every REFETCH_AT_MOST; whoever comes while one runs waits for it.
        if self._claim_read():
            try:
                await self._read()
            except KeysUnavailable:
                if self._read_at is None:
                    raise
        else:
            async with self._lock:
                pass
        if self._read_at is None:
            raise KeysUnavailable("no keys read yet")
        key = self._keys.get(kid)
        if key is None:
            raise InvalidToken("signed with a key pacod-id does not publish")
        return key

    async def verify(self, token: str) -> AccessToken:
        try:
            header = jwt.get_unverified_header(token)
        except jwt.PyJWTError as e:
            raise InvalidToken(str(e)) from e
        if header.get("typ") != "at+jwt":
            raise InvalidToken("not an access token")
        if header.get("alg") not in ("RS256", "ES256") or "crit" in header:
            raise InvalidToken("an algorithm or header this product does not take")
        key = await self._key(str(header.get("kid", "")))
        config = self._client.config
        try:
            claims = jwt.decode(
                token,
                key=key,
                algorithms=[header["alg"]],
                audience=config.audience,
                issuer=list(config.issuers),
                leeway=LEEWAY,
                options={"require": ["exp", "iat", "sub", "iss", "aud"]},
            )
        except jwt.ExpiredSignatureError as e:
            raise InvalidToken("expired") from e
        except jwt.PyJWTError as e:
            raise InvalidToken(str(e)) from e
        kind = claims.get("pid_kind")
        if kind not in ("user", "service"):
            raise InvalidToken("no subject kind")
        sid = claims.get("sid")
        if kind == "user" and not isinstance(sid, str):
            raise InvalidToken("a person's token without a session")
        act = claims.get("act")
        return AccessToken(
            sub=str(claims["sub"]),
            kind=kind,
            sid=sid if isinstance(sid, str) else None,
            client_id=str(claims.get("client_id", "")),
            scope=str(claims.get("scope", "")),
            issuer=str(claims["iss"]),
            actor=act.get("sub") if isinstance(act, dict) else None,
            expires_at=int(claims["exp"]),
        )
