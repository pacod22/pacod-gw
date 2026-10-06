"""The test cases: the built-in suites shipped with the tester, and suites admins uploaded."""

from dataclasses import dataclass
from pathlib import Path
from typing import Any

import yaml
from pydantic import ValidationError

from .expect import KNOWN_OPS
from .model import Case, Step, Suite

BUILTIN_DIR = Path(__file__).parent / "cases"
MAX_UPLOAD_BYTES = 256 * 1024


class CatalogError(Exception):
    """A suite file that cannot be used, with every reason."""

    def __init__(self, errors: list[str]):
        super().__init__("; ".join(errors))
        self.errors = errors


@dataclass
class Entry:
    suite: Suite
    source: str  # "builtin" or "uploaded"
    file: Path


def _location(loc: tuple[Any, ...]) -> str:
    return ".".join(str(p) for p in loc) or "(top)"


def _check_ops(step: Step, where: str) -> list[str]:
    errors = []
    specs: list[tuple[str, Any]] = [
        *((f"headers.{k}", v) for k, v in step.expect.headers.items()),
        *((f"json.{k}", v) for k, v in step.expect.json_paths.items()),
        *((f"sse_last.{k}", v) for k, v in step.expect.sse_last.items()),
    ]
    if step.expect.body is not None:
        specs.append(("body", step.expect.body))
    for name, spec in specs:
        if isinstance(spec, dict):
            unknown = sorted(set(spec) - KNOWN_OPS)
            if unknown:
                errors.append(f"{where}.expect.{name}: unknown check {', '.join(unknown)}")
    for source in step.save.values():
        if not (source == "status" or source.startswith(("json.", "header."))):
            errors.append(f"{where}.save: {source!r} is not status, json.<path> or header.<name>")
    return errors


def parse(text: str) -> Suite:
    """A suite from YAML, or a CatalogError with every problem."""
    try:
        data = yaml.safe_load(text)
    except yaml.YAMLError as e:
        raise CatalogError([f"not YAML: {e}"]) from None
    if not isinstance(data, dict):
        raise CatalogError(["a suite file is a mapping with suite, title and cases"])
    try:
        suite = Suite.model_validate(data)
    except ValidationError as e:
        raise CatalogError([f"{_location(err['loc'])}: {err['msg']}" for err in e.errors()]) from None
    errors: list[str] = []
    for i, step in enumerate(suite.setup):
        errors += _check_ops(step, f"setup.{i}")
    for case in suite.cases:
        for i, step in enumerate(case.steps):
            errors += _check_ops(step, f"cases.{case.id}.steps.{i}")
    if errors:
        raise CatalogError(errors)
    return suite


class Catalog:
    def __init__(self, upload_dir: Path, builtin_dir: Path = BUILTIN_DIR):
        self.upload_dir = upload_dir
        self.builtin_dir = builtin_dir
        self.builtin: list[Entry] = [
            Entry(parse(f.read_text()), "builtin", f) for f in sorted(builtin_dir.glob("*.yaml"))
        ]
        names = [e.suite.suite for e in self.builtin]
        if len(set(names)) != len(names):
            raise CatalogError(["two built-in suites share a name"])

    def uploaded(self) -> list[Entry]:
        """The uploaded suites. A file that no longer parses (after an upgrade changed the format)
        is left out rather than breaking the catalog."""
        out = []
        if self.upload_dir.is_dir():
            for f in sorted(self.upload_dir.glob("*.yaml")):
                try:
                    out.append(Entry(parse(f.read_text()), "uploaded", f))
                except (CatalogError, OSError):
                    continue
        return out

    def entries(self) -> list[Entry]:
        return self.builtin + self.uploaded()

    def case_ids(self, exclude_suite: str | None = None) -> dict[str, str]:
        """Case id to suite name, over the whole catalog."""
        return {
            c.id: e.suite.suite
            for e in self.entries()
            if e.suite.suite != exclude_suite
            for c in e.suite.cases
        }

    def upload(self, text: str) -> Suite:
        if len(text.encode()) > MAX_UPLOAD_BYTES:
            raise CatalogError([f"the file is larger than {MAX_UPLOAD_BYTES // 1024} KiB"])
        suite = parse(text)
        if any(e.suite.suite == suite.suite for e in self.builtin):
            raise CatalogError([f"suite {suite.suite!r} is built in; give the uploaded suite another name"])
        taken = self.case_ids(exclude_suite=suite.suite)
        clashes = [
            f"case {c.id!r} already exists in suite {taken[c.id]!r}" for c in suite.cases if c.id in taken
        ]
        if clashes:
            raise CatalogError(clashes)
        self.upload_dir.mkdir(parents=True, exist_ok=True)
        target = self.upload_dir / f"{suite.suite}.yaml"
        tmp = target.with_suffix(".tmp")
        tmp.write_text(text)
        tmp.replace(target)
        return suite

    def delete(self, name: str) -> bool:
        for e in self.uploaded():
            if e.suite.suite == name:
                e.file.unlink(missing_ok=True)
                return True
        return False

    def find(self, case_ids: list[str] | None) -> list[tuple[Entry, list[Case]]]:
        """The suites and cases of a run, in catalog order. None or empty means every case."""
        wanted = set(case_ids or [])
        out = []
        for e in self.entries():
            cases = [c for c in e.suite.cases if not wanted or c.id in wanted]
            if cases:
                out.append((e, cases))
        return out
