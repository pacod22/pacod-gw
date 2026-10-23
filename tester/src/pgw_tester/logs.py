"""The tester's log, after the pacod logging convention (pacod-demo/docs/LOGGING.md).

One event per line on standard output, in JSON (the default) or text:

    {"timestamp":"2026-10-23T06:41:07.695Z","level":"info","service":"gw-tester","logger":"pgw_tester","message":"..."}
    2026-10-23T06:41:07.695Z INFO  gw-tester pgw_tester: ...

``TESTER_LOG_LEVEL`` sets the level (debug, info, warn, error; any case, ``warning`` is ``warn``;
anything else is info), ``TESTER_LOG_FORMAT`` the format (json or text; anything else is json),
``SERVICE_NAME`` the service key (``gw-tester``). Fields passed with ``extra=`` are written flat
after the five fixed keys; an exception adds ``error`` and ``stack_trace``.
"""

from __future__ import annotations

import json
import logging
import os
import sys
import traceback
from datetime import UTC, datetime
from typing import Any

DEFAULT_SERVICE = "gw-tester"

# The keys every event starts with. An extra field of one of these names gets a trailing
# underscore, so that it cannot hide them.
RESERVED = ("timestamp", "level", "service", "logger", "message")

_LEVELS = {
    "debug": logging.DEBUG,
    "info": logging.INFO,
    "warn": logging.WARNING,
    "warning": logging.WARNING,
    "error": logging.ERROR,
}

# Everything a LogRecord carries by itself; anything else on a record came from `extra=`.
# uvicorn adds `color_message`, its message with ANSI colour codes, which no log should carry.
_STANDARD = set(logging.LogRecord("", 0, "", 0, "", None, None).__dict__) | {
    "message",
    "asctime",
    "taskName",
    "color_message",
}


def parse_level(value: str | None) -> tuple[int, str | None]:
    """The level a TESTER_LOG_LEVEL value stands for, and a warning when it stands for none."""
    if value is None or not value.strip():
        return logging.INFO, None
    level = _LEVELS.get(value.strip().lower())
    if level is None:
        return logging.INFO, f'TESTER_LOG_LEVEL: "{value}" is not one of debug, info, warn, error; using info'
    return level, None


def parse_format(value: str | None) -> tuple[str, str | None]:
    """json or text, and a warning when the value is neither."""
    if value is None or not value.strip():
        return "json", None
    fmt = value.strip().lower()
    if fmt in ("json", "text"):
        return fmt, None
    return "json", f'TESTER_LOG_FORMAT: "{value}" is not one of json, text; writing json'


def level_name(levelno: int) -> str:
    """Python's levels as the convention names them: warning is warn, critical is error."""
    if levelno >= logging.ERROR:
        return "error"
    if levelno >= logging.WARNING:
        return "warn"
    if levelno >= logging.INFO:
        return "info"
    if levelno >= logging.DEBUG:
        return "debug"
    return "trace"


def _timestamp(created: float) -> str:
    """RFC 3339 in UTC with milliseconds: 2026-10-23T06:41:07.695Z."""
    t = datetime.fromtimestamp(created, UTC)
    return t.strftime("%Y-%m-%dT%H:%M:%S.") + f"{t.microsecond // 1000:03d}Z"


def _jsonable(value: Any) -> Any:
    if value is None or isinstance(value, bool | int | float | str):
        return value
    if isinstance(value, list | tuple):
        return [_jsonable(v) for v in value]
    if isinstance(value, dict):
        return {str(k): _jsonable(v) for k, v in value.items()}
    return str(value)


class ConventionFormatter(logging.Formatter):
    """Formats a record as the event of the convention, in JSON or in text."""

    def __init__(self, service: str = DEFAULT_SERVICE, fmt: str = "json") -> None:
        super().__init__()
        self.service = service
        self.json = fmt == "json"

    def fields(self, record: logging.LogRecord) -> list[tuple[str, Any]]:
        out: list[tuple[str, Any]] = []
        for key, value in record.__dict__.items():
            if key in _STANDARD or key.startswith("_"):
                continue
            out.append((f"{key}_" if key in RESERVED else key, _jsonable(value)))
        if record.exc_info and record.exc_info[1] is not None:
            exc = record.exc_info[1]
            out.append(("error", f"{type(exc).__name__}: {exc}"))
            out.append(("stack_trace", "".join(traceback.format_exception(*record.exc_info)).rstrip("\n")))
        elif record.stack_info:
            out.append(("stack_trace", record.stack_info))
        return out

    def format(self, record: logging.LogRecord) -> str:
        message = record.getMessage() or record.name
        fields = self.fields(record)
        if self.json:
            event: dict[str, Any] = {
                "timestamp": _timestamp(record.created),
                "level": level_name(record.levelno),
                "service": self.service,
                "logger": record.name,
                "message": message,
            }
            for key, value in fields:
                event.setdefault(key, value)
            return json.dumps(event, ensure_ascii=False, separators=(",", ":"), default=str)
        line = (
            f"{_timestamp(record.created)} {level_name(record.levelno).upper():<5} "
            f"{self.service} {record.name}: {message}"
        )
        trace = None
        for key, value in fields:
            if key == "stack_trace":
                trace = value
                continue
            line += f" {key}={_text_value(value)}"
        # The stack trace follows on the next lines.
        return f"{line}\n{trace}" if trace else line


def _text_value(value: Any) -> str:
    if isinstance(value, bool):
        return "true" if value else "false"
    if value is None:
        return "null"
    if isinstance(value, int | float):
        return str(value)
    text = value if isinstance(value, str) else json.dumps(value, ensure_ascii=False, default=str)
    if not text or any(c.isspace() or c in '"=' or not c.isprintable() for c in text):
        return json.dumps(text, ensure_ascii=False)
    return text


def setup(environ: dict[str, str] | None = None) -> None:
    """Sets up the root logger from the environment: one handler on standard output."""
    env = os.environ if environ is None else environ
    level, level_warning = parse_level(env.get("TESTER_LOG_LEVEL"))
    fmt, format_warning = parse_format(env.get("TESTER_LOG_FORMAT"))
    service = (env.get("SERVICE_NAME") or "").strip() or DEFAULT_SERVICE
    handler = logging.StreamHandler(sys.stdout)
    handler.setFormatter(ConventionFormatter(service, fmt))
    logging.basicConfig(level=level, handlers=[handler], force=True)
    log = logging.getLogger("pgw_tester")
    for warning in (level_warning, format_warning):
        if warning:
            log.warning(warning)
