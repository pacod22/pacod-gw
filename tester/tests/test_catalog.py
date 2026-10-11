"""The built-in catalog and uploads."""

from pathlib import Path

import pytest

from pgw_tester.catalog import Catalog, CatalogError, parse

SUITE = """
suite: extra
title: Extra
cases:
  - id: extra-health
    title: health
    steps:
      - call: { path: /healthz, auth: none }
"""


def test_builtin_catalog_is_valid(tmp_path: Path):
    catalog = Catalog(tmp_path)
    names = [e.suite.suite for e in catalog.builtin]
    assert names == ["health", "auth", "proxy", "managed", "limits", "routing", "observability", "providers"]
    ids = [c.id for e in catalog.builtin for c in e.suite.cases]
    assert len(ids) == len(set(ids))


@pytest.mark.parametrize(
    ("text", "fragment"),
    [
        ("- not a mapping", "a suite file is a mapping"),
        ("suite: x\ntitle: x\ncases: []", "cases"),
        ("suite: Bad Name\ntitle: x\ncases: [{id: a, title: a, steps: [{sleep: 1}]}]", "suite"),
        (
            "suite: x\ntitle: x\ncases: [{id: a, title: a, steps: [{sleep: 1, call: {path: /}}]}]",
            "exactly one",
        ),
        (
            "suite: x\ntitle: x\ncases: [{id: a, title: a,"
            " steps: [{call: {path: /}, expect: {body: {nearly: 1}}}]}]",
            "unknown check nearly",
        ),
        (
            "suite: x\ntitle: x\ncases: [{id: a, title: a, steps: [{call: {path: /}, save: {v: body}}]}]",
            "save",
        ),
        ("suite: x\ntitle: x\ncases: [{id: a, title: a, steps: [{sleep: soon}]}]", "duration"),
        ("suite: x\ntitle: x\nextra: 1\ncases: [{id: a, title: a, steps: [{sleep: 1}]}]", "extra"),
        (
            "suite: x\ntitle: x\ncases: [{id: a, title: a, steps: [{call: {path: /, auth: session}}]}]",
            "mgmt steps only",
        ),
        (
            "suite: x\ntitle: x\ncases: [{id: a, title: a, steps: [{mock: {path: /, auth: session}}]}]",
            "mgmt steps only",
        ),
    ],
)
def test_invalid_suites_explain_themselves(text: str, fragment: str):
    with pytest.raises(CatalogError) as e:
        parse(text)
    assert fragment in " ".join(e.value.errors)


def test_upload_and_delete(tmp_path: Path):
    catalog = Catalog(tmp_path / "cases")
    catalog.upload(SUITE)
    assert [e.suite.suite for e in catalog.uploaded()] == ["extra"]
    assert catalog.find(["extra-health"])[0][0].source == "uploaded"
    catalog.upload(SUITE.replace("title: health", "title: health again"))  # replacing is fine
    assert catalog.delete("extra")
    assert not catalog.delete("extra")
    assert not catalog.delete("health")  # built in


def test_upload_refuses_clashes(tmp_path: Path):
    catalog = Catalog(tmp_path / "cases")
    with pytest.raises(CatalogError, match="built in"):
        catalog.upload(SUITE.replace("suite: extra", "suite: health"))
    with pytest.raises(CatalogError, match="already exists"):
        catalog.upload(SUITE.replace("id: extra-health", "id: data-plane-live"))
