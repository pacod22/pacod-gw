"""The internal API of pacod-id for a product's Python service, with its client credentials.

Every call goes to pacod-id's internal address (``internal_url``); browsers use the public issuer.
"""

from __future__ import annotations

import base64
import time
from dataclasses import dataclass, field
from typing import Any
from urllib.parse import quote

import httpx

TOKEN_EXCHANGE = "urn:ietf:params:oauth:grant-type:token-exchange"
ACCESS_TOKEN = "urn:ietf:params:oauth:token-type:access_token"
REFRESH_TOKEN = "urn:ietf:params:oauth:token-type:refresh_token"


@dataclass(frozen=True)
class IdConfig:
    """Where and as whom a product's service talks to pacod-id."""

    #: The public issuers whose access tokens the product takes (one per realm), as ``iss`` has them.
    issuers: tuple[str, ...]
    #: pacod-id's internal listener as the service reaches it (``http://id:8071``).
    internal_url: str
    client_id: str
    client_secret: str
    #: The product (``gw``) and the audience of its access tokens (``pacod-gw``).
    product: str
    audience: str


class ApiError(Exception):
    """pacod-id refused, with its error code, or could not be reached (``status`` 0)."""

    def __init__(self, status: int, code: str, message: str = "") -> None:
        super().__init__(f"pacod-id answered {status}: {code}: {message}")
        self.status = status
        self.code = code
        self.message = message


@dataclass
class Tokens:
    access_token: str
    refresh_token: str | None = None
    expires_in: int | None = None
    #: When they were issued (monotonic seconds), for refreshing in time.
    received_at: float = field(default_factory=time.monotonic)


def _basic(client_id: str, secret: str) -> str:
    # RFC 6749 2.3.1: form-encoded before Base64.
    pair = f"{quote(client_id, safe='')}:{quote(secret, safe='')}"
    return "Basic " + base64.b64encode(pair.encode()).decode()


def _read(r: httpx.Response) -> Any:
    try:
        body = r.json()
    except ValueError:
        body = None
    if r.is_success:
        return body
    err = (body or {}).get("error") if isinstance(body, dict) else None
    if isinstance(err, dict):
        code = (err.get("details") or {}).get("code") or err.get("type") or "error"
        message = err.get("message", "")
    else:
        code = err or "error"
        message = (body or {}).get("error_description", "") if isinstance(body, dict) else ""
    raise ApiError(r.status_code, str(code), str(message))


class IdClient:
    """The internal API. One per process; close it with :meth:`aclose`."""

    def __init__(self, config: IdConfig, http: httpx.AsyncClient | None = None) -> None:
        self.config = config
        # Longer than the feed's 25-second wait.
        self.http = http or httpx.AsyncClient(timeout=40, follow_redirects=False)
        self._service: tuple[str, float] | None = None

    async def aclose(self) -> None:
        await self.http.aclose()

    def url(self, path: str) -> str:
        return self.config.internal_url.rstrip("/") + path

    async def _token(self, form: dict[str, str]) -> Tokens:
        try:
            r = await self.http.post(
                self.url("/oidc/token"),
                data=form,
                headers={"authorization": _basic(self.config.client_id, self.config.client_secret)},
            )
        except httpx.HTTPError as e:
            raise ApiError(0, "unreachable", str(e)) from e
        body = _read(r)
        return Tokens(body["access_token"], body.get("refresh_token"), body.get("expires_in"))

    async def service_token(self) -> str:
        """The service's own token (client credentials), cached until 30 seconds before expiry."""
        if self._service and self._service[1] > time.monotonic():
            return self._service[0]
        t = await self._token({"grant_type": "client_credentials"})
        self._service = (t.access_token, time.monotonic() + max(5, (t.expires_in or 300) - 30))
        return t.access_token

    async def _as_service(self, method: str, path: str, **kwargs: Any) -> httpx.Response:
        """A request with the service's token; when pacod-id refuses the cached one (its keys
        rotated at once, say), once more with a new token rather than failing until it expires."""
        for attempt in (1, 2):
            token = await self.service_token()
            try:
                r = await self.http.request(
                    method, self.url(path), headers={"authorization": f"Bearer {token}"}, **kwargs
                )
            except httpx.HTTPError as e:
                raise ApiError(0, "unreachable", str(e)) from e
            if r.status_code != 401 or attempt == 2:
                return r
            self._service = None
        raise AssertionError("unreachable")

    async def call(self, method: str, path: str, json: Any = None) -> Any:
        return _read(await self._as_service(method, path, json=json))

    async def exchange_for_admin(self, subject_token: str) -> Tokens:
        """A 60-second token for pacod-id's admin API acting for the caller of ``subject_token``."""
        return await self._token(
            {
                "grant_type": TOKEN_EXCHANGE,
                "subject_token": subject_token,
                "subject_token_type": ACCESS_TOKEN,
                "audience": "pacod-id",
            }
        )

    async def delegated_grant(self, subject_token: str) -> Tokens:
        """A refresh token acting for a person, bound to their session, for a long task."""
        return await self._token(
            {
                "grant_type": TOKEN_EXCHANGE,
                "subject_token": subject_token,
                "subject_token_type": ACCESS_TOKEN,
                "requested_token_type": REFRESH_TOKEN,
            }
        )

    async def refresh(self, refresh_token: str) -> Tokens:
        return await self._token({"grant_type": "refresh_token", "refresh_token": refresh_token})

    async def revoke(self, refresh_token: str) -> None:
        """Revokes a refresh token of this client (RFC 7009); always succeeds when pacod-id answers."""
        try:
            r = await self.http.post(
                self.url("/oidc/revoke"),
                data={"token": refresh_token},
                headers={"authorization": _basic(self.config.client_id, self.config.client_secret)},
            )
        except httpx.HTTPError as e:
            raise ApiError(0, "unreachable", str(e)) from e
        _read(r)

    async def revoke_session(self, sid: str) -> None:
        await self.call("POST", f"/internal/v1/sessions/{quote(sid, safe='')}/revoke")

    async def introspect(self, token: str) -> dict[str, Any]:
        return _read(await self._as_service("POST", "/internal/v1/introspect", data={"token": token}))

    async def snapshot(self) -> dict[str, Any]:
        return await self.call("GET", f"/internal/v1/authz/snapshot?product={self.config.product}")

    async def changes(self, after: int, wait: int = 25) -> dict[str, Any]:
        path = f"/internal/v1/authz/changes?product={self.config.product}&after={after}&wait={wait}"
        return await self.call("GET", path)

    async def deletion_done(self, sub: str) -> None:
        await self.call(
            "POST", f"/internal/v1/deletions/{quote(sub, safe='')}/done?product={self.config.product}"
        )
