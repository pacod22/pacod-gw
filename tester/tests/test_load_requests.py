"""What a load test sends, how it is split, and what a request must be."""

import json

import pytest
from pydantic import ValidationError

from pgw_tester.load.model import LoadRequest, LoadShape, key_hint
from pgw_tester.load.requests import auth_header, build
from pgw_tester.load.split import shares, split_int

SHAPE = {"mode": "concurrency", "stages": [{"duration_s": 0, "target": 5}, {"duration_s": 10, "target": 5}]}


def req(**over) -> LoadRequest:
    data = {"target": "mock", "mock": {}, "load": SHAPE, "agents": ["a"], **over}
    return LoadRequest.model_validate(data)


@pytest.mark.parametrize(
    ("interface", "path", "field"),
    [
        ("openai", "/v1/chat/completions", "messages"),
        ("anthropic", "/v1/messages", "max_tokens"),
        ("native", "/gw/v1/chat", "max_output_tokens"),
        ("gemini", "/v1beta/models/m/x:streamGenerateContent?alt=sse", "contents"),
    ],
)
def test_managed_bodies_follow_each_interface(interface, path, field):
    family = "gemini" if interface == "gemini" else "anthropic" if interface == "anthropic" else "openai"
    spec = build(req(interface=interface, mock={"family": family}), "m/x")
    assert spec.path == path
    body = json.loads(spec.body)
    assert field in body
    assert spec.stream
    if interface == "openai":
        assert body["stream_options"] == {"include_usage": True}
    if interface == "anthropic":
        assert spec.headers["anthropic-version"]


def test_proxy_paths_name_the_provider_and_the_upstream_id():
    spec = build(
        req(mode="proxy", stream=False), "tester-load-mock/gpt-mock", "tester-load-mock-openai", "gpt-mock"
    )
    assert spec.path == "/proxy/tester-load-mock-openai/v1/chat/completions"
    assert json.loads(spec.body)["model"] == "gpt-mock"
    assert "x-gw-retries" not in spec.headers, "a mock target keeps the gateway's retries"


def test_embeddings_are_never_streamed_and_paid_targets_make_one_attempt():
    r = req(
        target="own",
        mock=None,
        own={"secret": "pgw_k_secretsecret", "model": "acme/embed"},
        interface="embeddings",
        load={**SHAPE, "max_calls": 50},
        acknowledge_cost=True,
    )
    spec = build(r, "acme/embed")
    assert (spec.path, spec.stream, json.loads(spec.body)["input"][0][:4]) == (
        "/v1/embeddings",
        False,
        "word",
    )
    assert spec.headers["x-gw-retries"] == "1"


def test_the_key_goes_where_each_sdk_puts_it():
    assert [auth_header(i) for i in ("openai", "anthropic", "gemini", "native")] == [
        "authorization",
        "x-api-key",
        "x-goog-api-key",
        "authorization",
    ]


def test_shares_add_up_to_the_whole():
    assert split_int(10, 3) == [4, 3, 3]
    shape = LoadShape.model_validate(
        {"mode": "rate", "stages": [{"duration_s": 5, "target": 10}], "max_in_flight": 5}
    )
    parts = shares(shape, 2, 101)
    assert [p.stages[0].target for p in parts] == [5.0, 5.0]
    assert [p.max_in_flight for p in parts] == [3, 2]
    assert [p.max_calls for p in parts] == [51, 50]
    conc = shares(LoadShape.model_validate(SHAPE), 3, None)
    assert [p.stages[1].target for p in conc] == [2, 2, 1]
    assert all(p.max_calls is None for p in conc)


def test_requests_that_cannot_run_are_refused():
    with pytest.raises(ValidationError, match="native interface"):
        req(interface="native", mode="proxy")
    with pytest.raises(ValidationError, match="own API"):
        req(interface="anthropic", mode="proxy")
    with pytest.raises(ValidationError, match="options"):
        req(target="live", mock=None)
    with pytest.raises(ValidationError, match="rate mode"):
        req(load={"mode": "rate", "stages": [{"duration_s": 5, "target": 1}]})
    with pytest.raises(ValidationError, match="named twice"):
        req(agents=["a", "a"])
    external = {"secret": "k-123456789", "model": "m"}
    for url, says in [
        ("ftp://x", "http"),
        ("http://u:p@lite:14080", "user name"),
        ("http://lite/?a=1", "query"),
    ]:
        with pytest.raises(ValidationError, match=says):
            req(target="external", mock=None, external={**external, "base_url": url})
    with pytest.raises(ValidationError, match="provider's name"):
        req(target="external", mock=None, mode="proxy", external={**external, "base_url": "http://lite"})


def test_a_run_keeps_a_hint_of_a_pasted_key_and_never_the_key():
    r = req(
        target="external",
        mock=None,
        external={
            "secret": "pgw_dev_DevelopmentOnlyKey0123456789",
            "model": "m",
            "base_url": "http://lite:8080/",
        },
    )
    public = r.public()
    assert public["external"]["key_hint"] == "pgw_dev_…"
    assert public["external"]["base_url"] == "http://lite:8080"
    assert "DevelopmentOnlyKey" not in json.dumps(public)
    assert key_hint("sk-other-shape-123").startswith("sha256:")
