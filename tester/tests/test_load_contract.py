"""The agents' contract fixtures (`crates/pacod-gw-load/tests/contract/`), which the agent's own
tests read too: every one reads into the tester's model and writes back the same JSON, and the
histogram vectors hold."""

import json
from pathlib import Path

import pytest

from pgw_tester.load.contract import ErrorBody, Info, Prepare, Prepared, Report
from pgw_tester.load.histogram import Histogram, bounds, index

CONTRACT = Path(__file__).resolve().parents[2] / "crates" / "pacod-gw-load" / "tests" / "contract"


def fixture(name: str):
    return json.loads((CONTRACT / name).read_text())


@pytest.mark.parametrize(
    ("name", "model"),
    [
        ("info.json", Info),
        ("prepared.json", Prepared),
        ("report.json", Report),
        ("error.json", ErrorBody),
    ],
)
def test_fixtures_read_and_write_back_unchanged(name, model):
    original = fixture(name)
    assert model.model_validate(original).model_dump(mode="json") == original


@pytest.mark.parametrize("name", ["prepare.json", "prepare-external.json"])
def test_prepare_writes_the_agents_json(name):
    original = fixture(name)
    plan = Prepare.model_validate(original)
    assert plan.wire() == original
    assert "secret" not in repr(plan) and "DevelopmentOnlyKey" not in repr(plan)


def test_the_histogram_vectors_hold():
    v = fixture("histogram.json")
    for value, i in v["index"]:
        assert index(value) == i, value
    for i, lo, hi in v["bounds"]:
        assert bounds(i) == (lo, hi), i
    h = Histogram.of(v["histogram"])
    for q, expected in v["percentiles"]:
        assert h.percentile(q) == expected, q


def test_histograms_add_up():
    a = Histogram.of({"n": 2, "min": 150, "max": 300, "sum": 450, "b": [[139, 1], [203, 1]]})
    b = Histogram.of({"n": 1, "min": 90, "max": 90, "sum": 90, "b": [[90, 1]]})
    a.merge(b)
    assert (a.n, a.min, a.max, a.sum) == (3, 90, 300, 540)
    assert a.percentile(0.0) == 90
    assert Histogram().percentile(0.5) == 0
