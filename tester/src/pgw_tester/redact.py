"""Keeps secrets out of everything a run records. The session token, the scratch keys and the
credential secrets of a run are registered here; gateway keys and tokens are also caught by
their shape."""

import re

MARK = "[redacted]"
# pgw_<id>_<secret>, pgwa_… and pgws_…: keep the prefix and id, drop the secret.
_GATEWAY_SECRET = re.compile(r"\b(pgw[as]?_[A-Za-z0-9-]+_)[A-Za-z0-9]{8,}")
SECRET_HEADERS = {"authorization", "x-api-key", "x-goog-api-key", "proxy-authorization", "cookie"}


class Redactor:
    def __init__(self, secrets: list[str] | None = None):
        self._secrets: list[str] = []
        for s in secrets or []:
            self.add(s)

    def add(self, secret: str | None) -> None:
        # Short values would mask ordinary words.
        if secret and len(secret) >= 8 and secret not in self._secrets:
            self._secrets.append(secret)
            self._secrets.sort(key=len, reverse=True)

    def leaks(self, text: str) -> bool:
        """Whether a text contains a registered secret."""
        return any(s in text for s in self._secrets)

    def __call__(self, text: str) -> str:
        for s in self._secrets:
            text = text.replace(s, MARK)
        return _GATEWAY_SECRET.sub(lambda m: m[1] + MARK, text)

    def headers(self, headers: dict[str, str]) -> dict[str, str]:
        return {k: MARK if k.lower() in SECRET_HEADERS else self(v) for k, v in headers.items()}
