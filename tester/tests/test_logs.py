"""The tester's log follows the pacod logging convention (logs.py)."""

import json
import logging
import re

from pgw_tester.logs import ConventionFormatter, level_name, parse_format, parse_level

TS = re.compile(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$")


def record(msg="started %s", args=("run-1",), level=logging.INFO, exc_info=None, **extra):
    r = logging.LogRecord("pgw_tester", level, __file__, 1, msg, args, exc_info)
    for k, v in extra.items():
        setattr(r, k, v)
    return r


def test_json_has_the_five_keys_first_and_the_extras_flat():
    line = ConventionFormatter("gw-tester", "json").format(record(run_id="r1", cases=3, ok=True))
    event = json.loads(line)
    assert list(event)[:5] == ["timestamp", "level", "service", "logger", "message"]
    assert TS.match(event["timestamp"]), event["timestamp"]
    assert (event["level"], event["service"], event["logger"]) == ("info", "gw-tester", "pgw_tester")
    assert event["message"] == "started run-1"
    assert (event["run_id"], event["cases"], event["ok"]) == ("r1", 3, True)
    assert "\n" not in line


def test_reserved_extras_get_a_trailing_underscore():
    event = json.loads(ConventionFormatter("gw-tester", "json").format(record(service="other", level_="x")))
    assert event["service"] == "gw-tester"
    assert event["service_"] == "other"


def test_an_exception_adds_error_and_stack_trace():
    try:
        raise ValueError("bad input")
    except ValueError:
        import sys

        r = record("run failed", (), logging.ERROR, sys.exc_info())
    event = json.loads(ConventionFormatter("gw-tester", "json").format(r))
    assert event["level"] == "error"
    assert event["error"] == "ValueError: bad input"
    assert "Traceback" in event["stack_trace"] and "\n" in event["stack_trace"]

    text = ConventionFormatter("gw-tester", "text").format(r)
    first, rest = text.split("\n", 1)
    assert first.endswith('gw-tester pgw_tester: run failed error="ValueError: bad input"')
    assert rest.startswith("Traceback")


def test_text_is_one_line_in_the_fixed_order_without_colour():
    line = ConventionFormatter("gw-tester", "text").format(
        record("grant not revoked", (), logging.WARNING, path="/a b", n=2)
    )
    ts, rest = line.split(" ", 1)
    assert TS.match(ts)
    assert rest == 'WARN  gw-tester pgw_tester: grant not revoked path="/a b" n=2'
    assert "\x1b" not in line
    # uvicorn's coloured copy of the message is left out.
    uvicorn = ConventionFormatter("gw-tester", "json").format(record(color_message="\x1b[36mx\x1b[0m"))
    assert "color_message" not in uvicorn


def test_levels_and_formats_read_as_the_convention_says():
    assert parse_level("DEBUG") == (logging.DEBUG, None)
    assert parse_level("Warning") == (logging.WARNING, None)
    assert parse_level("warn") == (logging.WARNING, None)
    assert parse_level(None) == (logging.INFO, None)
    level, warning = parse_level("verbose")
    assert level == logging.INFO and "verbose" in warning
    assert parse_format("TEXT") == ("text", None)
    assert parse_format("") == ("json", None)
    fmt, warning = parse_format("xml")
    assert fmt == "json" and "xml" in warning
    assert [level_name(n) for n in (10, 20, 30, 40, 50)] == ["debug", "info", "warn", "error", "error"]
