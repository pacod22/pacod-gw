"""The admin's session for work that outlasts a request: with pacod-id, a delegated grant of
the session, since the token the console forwards ends after five
minutes. The grant ends when the admin signs out, and is revoked when the work is done."""

import logging

import httpx
from pacod_id.client import ApiError, IdClient, IdConfig
from pacod_id.grant import DelegatedGrant, SignedOut

from .config import Settings

log = logging.getLogger("pgw_tester")


class NoGrant(Exception):
    """pacod-id did not answer the request for a delegated grant."""


def identity_client(settings: Settings, transport: httpx.AsyncBaseTransport | None = None) -> IdClient | None:
    if not settings.delegated:
        return None
    return IdClient(
        IdConfig(
            issuers=(settings.id_issuer,) if settings.id_issuer else (),
            internal_url=settings.id_internal_url,
            client_id=settings.id_client_id,
            client_secret=settings.id_client_secret,
            product="gw",
            audience="pacod-gw",
        ),
        httpx.AsyncClient(transport=transport, timeout=15, follow_redirects=False),
    )


async def take_grant(identity: IdClient | None, token: str) -> DelegatedGrant | None:
    """A grant of the session behind `token`, or None without pacod-id."""
    if identity is None:
        return None
    try:
        return await DelegatedGrant.start(identity, token)
    except SignedOut:
        raise
    except ApiError as e:
        raise NoGrant(f"pacod-id gave the run no grant of the session: {e}") from None


async def close_grant(grant: DelegatedGrant | None) -> None:
    """Revokes a grant; a failure is logged (pacod-id lets it expire)."""
    if grant is None:
        return
    try:
        await grant.close()
    except Exception:
        log.warning("the run's grant was not revoked; it expires on its own", exc_info=True)
