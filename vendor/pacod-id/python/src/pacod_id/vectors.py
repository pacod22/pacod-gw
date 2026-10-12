"""Runs a file of test vectors (policy/engine-vectors/README.md): the engine's own, or a product's
(``authz/vectors/*.yaml`` next to its manifest)."""

from collections.abc import Callable
from typing import Any

from pacod_id.authz import Catalog, HeldBinding, Manifest, Subject, load_yaml


def _one(c: Catalog, subject: Subject, check: dict[str, Any]) -> str | None:
    """One check: None when it holds, else the engine's answer."""
    expect = check["expect"]

    def boolean(got: bool) -> str | None:
        return None if got is expect else str(got)

    if "allowed" in check:
        return boolean(c.allowed(subject, check["allowed"], check["at"]))
    if "has_role" in check:
        return boolean(c.has_role(subject, check["has_role"], check["scope"]))
    if "holds_any_at" in check:
        return boolean(c.holds_any_at(subject, check["holds_any_at"]))
    if "allowed_somewhere" in check:
        return boolean(c.allowed_somewhere(subject, check["allowed_somewhere"]))
    if "touches" in check:
        return boolean(c.touches(subject, check["touches"]))
    if "can_grant" in check:
        got = c.can_grant(subject, check["can_grant"], check["at"]) or "ok"
        return None if got == expect else got
    if "check_protected" in check:
        p = check["check_protected"]
        before = [HeldBinding.of(b) for b in p["before"]]
        after = [HeldBinding.of(b) for b in p["after"]]
        v = c.check_protected(subject, before, after)
        if expect == "ok":
            return None if v is None else repr(v)
        want = expect["violation"]
        holds = v is not None and (v.role, v.scope) == (want["role"], want["scope"])
        return None if holds else repr(v)
    raise ValueError(f"unknown check {check}")


def run(text: str, load: Callable[[str], Manifest]) -> tuple[int, list[str]]:
    """Runs every check of a vector file. ``load`` reads a manifest by the name the file lists
    under ``manifests``. Returns the number of checks and every failure with its case."""
    file = load_yaml(text)
    catalog = Catalog(load(name) for name in file["manifests"])
    checks, failures = 0, []
    for case in file["cases"]:
        subject = Subject.of(case.get("subject") or case["actor"])
        for check in case["checks"]:
            checks += 1
            try:
                got = _one(catalog, subject, check)
            except (KeyError, ValueError, TypeError) as e:
                failures.append(f"{case['name']}: {e}")
                continue
            if got is not None:
                failures.append(f"{case['name']}: {check} gave {got}")
    return checks, failures
