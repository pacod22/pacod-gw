"""Checking a response against the expectations of a step."""

import json
import re
from dataclasses import dataclass, field
from typing import Any

import httpx

from .model import Expect
from .templating import get_path

TYPES: dict[str, tuple[type, ...]] = {
    "string": (str,),
    "number": (int, float),
    "integer": (int,),
    "boolean": (bool,),
    "object": (dict,),
    "array": (list,),
}


@dataclass
class Reply:
    status: int
    headers: httpx.Headers
    text: str
    elapsed_ms: float = 0.0
    _json: Any = field(default=None, repr=False)
    _parsed: bool = field(default=False, repr=False)

    def json(self) -> Any:
        """The body as JSON, or None when it is not JSON."""
        if not self._parsed:
            self._parsed = True
            try:
                self._json = json.loads(self.text) if self.text else None
            except ValueError:
                self._json = None
        return self._json

    def sse_last(self) -> Any:
        """The JSON of the last `data:` line of an event stream that is JSON."""
        for line in reversed(self.text.splitlines()):
            if line.startswith("data:"):
                payload = line[5:].strip()
                if payload == "[DONE]":
                    continue
                try:
                    return json.loads(payload)
                except ValueError:
                    continue
        return None


def _short(value: Any, limit: int = 120) -> str:
    text = value if isinstance(value, str) else json.dumps(value, ensure_ascii=False, default=str)
    return text if len(text) <= limit else text[:limit] + "…"


def _is_type(value: Any, name: str) -> bool:
    if name == "null":
        return value is None
    types = TYPES.get(name)
    if types is None:
        return False
    # bool is an int in Python; JSON keeps them apart.
    if isinstance(value, bool) and name in ("number", "integer"):
        return False
    return isinstance(value, types)


def _contains(haystack: Any, needle: Any) -> bool:
    if isinstance(haystack, str):
        return str(needle) in haystack
    if isinstance(haystack, list):
        return needle in haystack
    if isinstance(haystack, dict):
        return needle in haystack
    return False


def check(found: bool, value: Any, spec: Any) -> str | None:
    """None when the value satisfies the spec, otherwise why not. A spec that is not a mapping
    means `equals`."""
    if not isinstance(spec, dict):
        spec = {"equals": spec}
    for op, want in spec.items():
        if op in ("exists", "present"):
            if bool(want) != found:
                return "is missing" if want else f"should be absent but is {_short(value)}"
            continue
        if not found:
            return "is missing"
        problem = _apply(op, value, want)
        if problem:
            return problem
    return None


def _apply(op: str, value: Any, want: Any) -> str | None:
    match op:
        case "equals":
            # Headers are text: compare "1" and 1 alike.
            if value == want or (isinstance(value, str) and not isinstance(want, str) and value == str(want)):
                return None
            return f"is {_short(value)}, expected {_short(want)}"
        case "not_equals":
            return f"should not be {_short(want)}" if value == want else None
        case "contains":
            return None if _contains(value, want) else f"does not contain {_short(want)}: {_short(value)}"
        case "contains_all":
            missing = [w for w in want if not _contains(value, w)]
            return f"lacks {_short(missing)}: {_short(value)}" if missing else None
        case "not_contains":
            return f"contains {_short(want)}" if _contains(value, want) else None
        case "matches":
            return None if re.search(str(want), str(value)) else f"{_short(value)} does not match /{want}/"
        case "starts_with":
            return None if str(value).startswith(str(want)) else f"does not start with {_short(want)}"
        case "ends_with":
            return None if str(value).rstrip().endswith(str(want)) else f"does not end with {_short(want)}"
        case "type":
            return None if _is_type(value, str(want)) else f"is {_short(value)}, not of type {want}"
        case "one_of":
            return None if value in want else f"is {_short(value)}, expected one of {_short(want)}"
        case "gt" | "gte" | "lt" | "lte":
            try:
                number = float(value)
            except (TypeError, ValueError):
                return f"is {_short(value)}, not a number"
            ok = {
                "gt": number > want,
                "gte": number >= want,
                "lt": number < want,
                "lte": number <= want,
            }[op]
            return None if ok else f"is {number:g}, expected {op} {want}"
        case "length":
            try:
                n = len(value)
            except TypeError:
                return f"is {_short(value)}, which has no length"
            return check(True, n, want) and f"has length {n}, expected {_short(want)}"
    return f"has the unknown check {op!r}"


KNOWN_OPS = {
    "exists",
    "present",
    "equals",
    "not_equals",
    "contains",
    "contains_all",
    "not_contains",
    "matches",
    "starts_with",
    "ends_with",
    "type",
    "one_of",
    "gt",
    "gte",
    "lt",
    "lte",
    "length",
}


def evaluate(expect: Expect, reply: Reply) -> list[str]:
    """Every expectation the reply does not meet, as a sentence."""
    failures: list[str] = []
    if expect.status is None:
        if not 200 <= reply.status < 300:
            failures.append(f"status {reply.status}, expected 2xx: {_short(reply.text, 300)}")
    else:
        accepted = expect.status if isinstance(expect.status, list) else [expect.status]
        if reply.status not in accepted:
            want = " or ".join(str(s) for s in accepted)
            failures.append(f"status {reply.status}, expected {want}: {_short(reply.text, 300)}")
    for name, spec in expect.headers.items():
        value = reply.headers.get(name)
        problem = check(value is not None, value, spec)
        if problem:
            failures.append(f"header {name} {problem}")
    if expect.json_paths:
        body = reply.json()
        if body is None and reply.text:
            failures.append(f"the body is not JSON: {_short(reply.text, 300)}")
        else:
            for path, spec in expect.json_paths.items():
                found, value = get_path(body, path)
                problem = check(found, value, spec)
                if problem:
                    failures.append(f"json {path} {problem}")
    if expect.body is not None:
        problem = check(True, reply.text, expect.body)
        if problem:
            failures.append(f"body {problem}")
    if expect.sse_last:
        last = reply.sse_last()
        if last is None:
            failures.append("the event stream has no JSON data line")
        else:
            for path, spec in expect.sse_last.items():
                found, value = get_path(last, path)
                problem = check(found, value, spec)
                if problem:
                    failures.append(f"last event {path} {problem}")
    for event in expect.sse_events:
        if f"event: {event}" not in reply.text and f"event:{event}" not in reply.text:
            failures.append(f"the event stream has no {event} event")
    return failures
