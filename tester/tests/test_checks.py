"""Expectations, variables and redaction."""

import httpx
import pytest

from pgw_tester.expect import Reply, check, evaluate
from pgw_tester.model import Expect, parse_duration
from pgw_tester.redact import Redactor
from pgw_tester.templating import MissingVariable, get_path, render


def reply(status=200, headers=None, body="") -> Reply:
    return Reply(status=status, headers=httpx.Headers(headers or {}), text=body)


def test_render_keeps_types_and_fills_text():
    v = {"org": "o1", "n": 3, "model": {"openai": "m/gpt"}}
    assert render("{{n}}", v) == 3
    assert render("/orgs/{{org}}/x", v) == "/orgs/o1/x"
    assert render({"model": "{{model.openai}}", "list": ["{{org}}"]}, v) == {"model": "m/gpt", "list": ["o1"]}
    with pytest.raises(MissingVariable):
        render("{{nope}}", v)


def test_paths_walk_lists_and_stars():
    data = {"data": [{"id": "a"}, {"id": "b"}], "x": {"y": None}}
    assert get_path(data, "data.1.id") == (True, "b")
    assert get_path(data, "data.-1.id") == (True, "b")
    assert get_path(data, "data.*.id") == (True, ["a", "b"])
    assert get_path(data, "x.y") == (True, None)
    assert get_path(data, "x.z")[0] is False
    assert get_path(data, "data.9")[0] is False


@pytest.mark.parametrize(
    ("value", "spec", "ok"),
    [
        ("abc", "abc", True),
        ("1", 1, True),  # headers are text
        ("abc", {"contains": "b"}, True),
        (["a", "b"], {"contains_all": ["a", "b"]}, True),
        (["a"], {"contains_all": ["a", "b"]}, False),
        ("abc", {"not_contains": "b"}, False),
        ("data: [DONE]\n\n", {"ends_with": "[DONE]"}, True),
        (1.5, {"type": "number"}, True),
        (True, {"type": "number"}, False),
        (3, {"gte": 1, "lt": 4}, True),
        ([1, 2], {"length": 2}, True),
        ([1, 2], {"length": {"gte": 3}}, False),
        ("x-1", {"matches": r"^x-\d$"}, True),
    ],
)
def test_check_ops(value, spec, ok):
    assert (check(True, value, spec) is None) is ok


def test_exists_and_missing():
    assert check(False, None, {"exists": False}) is None
    assert check(False, None, {"exists": True}) == "is missing"
    assert check(False, None, "x") == "is missing"


def test_evaluate_reports_every_failure():
    r = reply(402, {"x-gw-error-type": "quota_exceeded"}, '{"limits": [{"used": 1}]}')
    assert (
        evaluate(Expect.model_validate({"status": 402, "headers": {"x-gw-error-type": "quota_exceeded"}}), r)
        == []
    )
    assert evaluate(Expect(), r)[0].startswith("status 402, expected 2xx")
    failures = evaluate(
        Expect.model_validate({"status": 200, "json": {"limits.0.used": 2, "nope": {"exists": True}}}), r
    )
    assert len(failures) == 3


def test_event_streams():
    text = (
        'event: message.delta\ndata: {"a": 1}\n\n'
        'event: message.stop\ndata: {"usage": {"cost": {"amount": 0.1}}}\n\n'
    )
    expect = Expect.model_validate(
        {"sse_events": ["message.stop"], "sse_last": {"usage.cost.amount": {"type": "number"}}}
    )
    assert evaluate(expect, reply(body=text)) == []
    assert evaluate(Expect.model_validate({"sse_events": ["nope"]}), reply(body=text))


def test_durations():
    assert parse_duration("200ms") == pytest.approx(0.2)
    assert parse_duration("2s") == 2
    assert parse_duration("1m") == 60
    assert parse_duration(5) == 5
    with pytest.raises(ValueError):
        parse_duration("soon")


def test_redaction():
    r = Redactor(["session-token-123456"])
    r.add("short")  # too short to register
    text = "token session-token-123456, key pgw_abc123_ZZZZZZZZZZZZ, short"
    assert r(text) == "token [redacted], key pgw_abc123_[redacted], short"
    assert r.leaks("…session-token-123456…")
    assert r.headers({"Authorization": "Bearer x", "x-other": "session-token-123456"}) == {
        "Authorization": "[redacted]",
        "x-other": "[redacted]",
    }


def test_a_path_can_pick_a_list_element_by_a_member():
    from pgw_tester.templating import get_path

    content = [{"type": "thinking", "signature": "s"}, {"type": "tool_use", "id": "toolu_1"}]
    assert get_path({"content": content}, "content.[type=tool_use].id") == (True, "toolu_1")
    assert get_path({"content": content}, "content.[type=text].id") == (False, None)


def test_the_openai_slot_takes_every_provider_spoken_to_in_openais_dialect():
    from pgw_tester.fixtures import fits

    assert fits("openai", "openai", "openai_compatible")
    assert fits("openai", "ollama", "openai_compatible")
    assert fits("openai", "gemini", "openai_compatible")
    assert not fits("openai", "gemini", "native")
    assert fits("gemini", "gemini", "native")
    assert not fits("gemini", "gemini", "openai_compatible")
    assert fits("anthropic", "anthropic", "openai_compatible")
    assert not fits("anthropic", "openai", "openai_compatible")
