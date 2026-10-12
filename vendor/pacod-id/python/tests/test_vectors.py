"""Runs the shared engine vectors (policy/engine-vectors/), the specification every port of the
engine must pass."""

from pathlib import Path

import pytest

from pacod_id import vectors
from pacod_id.authz import Manifest, ManifestError, load_yaml

VECTORS = Path(__file__).resolve().parents[2] / "policy" / "engine-vectors"


def load(name: str) -> Manifest:
    return Manifest.from_yaml((VECTORS / "manifests" / f"{name}.yaml").read_text())


@pytest.mark.parametrize(
    ("file", "least"), [("evaluation.yaml", 51), ("grants.yaml", 21), ("protected.yaml", 8)]
)
def test_engine_vectors(file, least):
    checks, failures = vectors.run((VECTORS / file).read_text(), load)
    assert failures == []
    assert checks >= least


def test_the_runner_reports_failures():
    text = """
manifests: [acme]
cases:
  - name: wrong on purpose
    subject: { kind: user, bindings: [] }
    checks:
      - { allowed: acme.org.read, at: [platform], expect: true }
      - { can_grant: acme.viewer, at: [platform], expect: ok }
"""
    assert len(vectors.run(text, load)[1]) == 2


def test_manifest_errors():
    file = load_yaml((VECTORS / "manifest-errors.yaml").read_text())
    for case in file["cases"]:
        try:
            Manifest.from_source(case["manifest"])
            got = "ok"
        except ManifestError as e:
            got = e.code
        assert got == case["expect"], case["name"]


@pytest.mark.parametrize(
    "raw",
    [
        None,
        [],
        {"product": "p", "grant": {"default": "x.y"}, "extra": 1},
        {"product": "p", "roles": {"a": {"permissions": ["x.y"]}}, "grant": {"default": "x.y"}},
        {
            "product": "p",
            "roles": {"a": {"at": ["platform"], "implicit": "yes"}},
            "grant": {"default": "x.y"},
        },
    ],
)
def test_documents_of_the_wrong_shape_are_refused(raw):
    with pytest.raises(ManifestError) as e:
        Manifest.from_source(raw)
    assert e.value.code == "bad_manifest"
