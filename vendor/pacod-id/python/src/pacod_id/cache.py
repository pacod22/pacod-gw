"""The product's copy of what pacod-id decides, as an asyncio task: the snapshot,
then long-polled changes. Starting never waits for pacod-id: until the first snapshot,
``ready`` is false and ``feed_age`` is ``None``; a product refuses its admin surfaces then.
"""

from __future__ import annotations

import asyncio
import contextlib
import logging
import time
from collections.abc import Callable, Iterable
from dataclasses import dataclass, field
from typing import Any

from .authz import Binding
from .client import ApiError, IdClient

log = logging.getLogger("pacod_id")

#: Ended sessions are remembered this long: no access token outlives it.
REVOKED_SIDS_FOR = 15 * 60


@dataclass
class Copy:
    seq: int = 0
    bindings: dict[str, dict[str, Binding]] = field(default_factory=dict)
    disabled: set[str] = field(default_factory=set)
    deleted: set[str] = field(default_factory=set)
    revoked_sids: dict[str, float] = field(default_factory=dict)

    @classmethod
    def of(cls, snap: dict[str, Any]) -> Copy:
        c = cls(seq=int(snap["seq"]))
        for b in snap.get("bindings", []):
            c.add(b)
        c.disabled = set(snap.get("disabled", []))
        c.deleted = set(snap.get("deleted", []))
        now = time.time()
        c.revoked_sids = {r["sid"]: now for r in snap.get("revoked_sids", []) if r.get("sid")}
        return c

    def add(self, b: dict[str, Any]) -> None:
        if all(k in b for k in ("id", "principal", "role", "path")):
            self.bindings.setdefault(b["principal"], {})[b["id"]] = Binding(b["role"], tuple(b["path"]))

    def apply(self, e: dict[str, Any]) -> None:
        kind, sub = e.get("kind"), e.get("sub")
        if kind == "binding.added":
            self.add(e)
        elif kind == "binding.removed":
            held = self.bindings.get(e.get("principal", ""), {})
            held.pop(e.get("id", ""), None)
            if not held:
                self.bindings.pop(e.get("principal", ""), None)
        elif kind == "principal.disabled" and sub:
            self.disabled.add(sub)
        elif kind == "principal.enabled" and sub:
            self.disabled.discard(sub)
        elif kind == "principal.deleted" and sub:
            self.bindings.pop(sub, None)
            self.disabled.discard(sub)
            self.deleted.add(sub)
        elif kind == "session.revoked" and e.get("sid"):
            self.revoked_sids[e["sid"]] = time.time()
        self.seq = max(self.seq, int(e.get("seq", 0)))

    def prune(self) -> None:
        oldest = time.time() - REVOKED_SIDS_FOR
        self.revoked_sids = {k: v for k, v in self.revoked_sids.items() if v > oldest}


class AuthzCache:
    def __init__(self, client: IdClient) -> None:
        self._client = client
        self._copy = Copy()
        self._ready = False
        self._last_ok: float | None = None
        self._task: asyncio.Task[None] | None = None
        self._listeners: list[Callable[[dict[str, Any]], None]] = []
        self._seq_changed = asyncio.Event()

    def start(self) -> None:
        """Starts following the feed (in the FastAPI lifespan)."""
        if self._task is None:
            self._task = asyncio.create_task(self._follow())

    async def stop(self) -> None:
        if self._task:
            self._task.cancel()
            with contextlib.suppress(asyncio.CancelledError):
                await self._task
            self._task = None

    def on_event(self, listener: Callable[[dict[str, Any]], None]) -> None:
        """Called with each change (``principal.deleted``: delete the product's data)."""
        self._listeners.append(listener)

    @property
    def ready(self) -> bool:
        return self._ready

    @property
    def seq(self) -> int:
        return self._copy.seq

    @property
    def feed_age(self) -> float | None:
        return None if self._last_ok is None else time.monotonic() - self._last_ok

    def bindings_of(self, principal: str) -> tuple[Binding, ...]:
        return tuple(self._copy.bindings.get(principal, {}).values())

    def is_disabled(self, principal: str) -> bool:
        return principal in self._copy.disabled or principal in self._copy.deleted

    def sid_revoked(self, sid: str) -> bool:
        return sid in self._copy.revoked_sids

    async def wait_for(self, seq: int, timeout: float) -> bool:
        """Waits until the copy has ``seq`` (read your own writes)."""
        deadline = time.monotonic() + timeout
        while self._copy.seq < seq:
            left = deadline - time.monotonic()
            if left <= 0:
                return False
            self._seq_changed.clear()
            with contextlib.suppress(TimeoutError):
                await asyncio.wait_for(self._seq_changed.wait(), left)
        return True

    def _tell(self, events: Iterable[dict[str, Any]]) -> None:
        for e in events:
            for listener in self._listeners:
                try:
                    listener(e)
                except Exception:
                    log.exception("a listener of pacod-id's changes failed")

    def _store(self, copy: Copy) -> None:
        self._copy = copy
        self._last_ok = time.monotonic()
        self._seq_changed.set()

    async def _follow(self) -> None:
        backoff = 1.0
        while True:
            try:
                copy = Copy.of(await self._client.snapshot())
            except Exception as e:  # any answer it cannot read: the follower must not stop
                log.warning("pacod-id's snapshot cannot be read (%s); trying again", e)
                await asyncio.sleep(backoff)
                backoff = min(backoff * 2, 30)
                continue
            self._store(copy)
            self._ready = True
            backoff = 1.0
            # Deletions still waiting for this product's confirmation are told again on every
            # load (at start, after a gap): one missed while the product was down is not left.
            self._tell(
                {"seq": copy.seq, "kind": "principal.deleted", "sub": sub, "from_snapshot": True}
                for sub in sorted(copy.deleted)
            )
            while True:
                try:
                    answer = await self._client.changes(copy.seq, 25)
                    events = answer.get("events", [])
                    int(answer.get("seq", copy.seq))
                except ApiError as e:
                    if e.status == 410:
                        log.info("pacod-id no longer keeps the changes this copy needs; reading the snapshot")
                        break
                    log.warning("pacod-id's changes cannot be read (%s); trying again", e)
                    await asyncio.sleep(backoff)
                    backoff = min(backoff * 2, 30)
                    continue
                except Exception as e:  # an answer of another shape: the follower must not stop
                    log.warning("pacod-id's changes cannot be read (%s); trying again", e)
                    await asyncio.sleep(backoff)
                    backoff = min(backoff * 2, 30)
                    continue
                nxt = Copy(
                    seq=copy.seq,
                    bindings={k: dict(v) for k, v in copy.bindings.items()},
                    disabled=set(copy.disabled),
                    deleted=set(copy.deleted),
                    revoked_sids=dict(copy.revoked_sids),
                )
                for e in events:
                    nxt.apply(e)
                # The answer's seq: changes for other products move the cursor too.
                nxt.seq = max(nxt.seq, int(answer.get("seq", nxt.seq)))
                nxt.prune()
                copy = nxt
                self._store(copy)
                backoff = 1.0
                self._tell(events)
