"""Runs and traced messages on disk: one JSON file each, the newest `keep` of them."""

import json
import re
from collections.abc import Callable
from pathlib import Path
from typing import Any

from pydantic import BaseModel

from .results import FINISHED, Run, now

#: Run and message ids: the time they were made and six hex digits.
RUN_ID = re.compile(r"^[0-9]{8}-[0-9]{6}-[0-9a-f]{6}$")


class JsonStore[T: BaseModel]:
    def __init__(
        self,
        directory: Path,
        model: type[T],
        summary: Callable[[dict[str, Any]], dict[str, Any]],
        keep: int = 100,
    ):
        self.dir = directory
        self.model = model
        self.summary = summary
        self.keep = keep
        self.dir.mkdir(parents=True, exist_ok=True)

    def _file(self, item_id: str) -> Path | None:
        return self.dir / f"{item_id}.json" if RUN_ID.match(item_id) else None

    def save(self, item: T) -> None:
        target = self._file(item.id)  # type: ignore[attr-defined]
        assert target is not None
        tmp = target.with_suffix(".tmp")
        tmp.write_text(item.model_dump_json())
        tmp.replace(target)

    def load(self, item_id: str) -> T | None:
        f = self._file(item_id)
        if f is None or not f.is_file():
            return None
        try:
            return self.model.model_validate_json(f.read_text())
        except ValueError:
            return None

    def ids(self) -> list[str]:
        """Newest first: the ids start with the time."""
        return sorted((f.stem for f in self.dir.glob("*.json") if RUN_ID.match(f.stem)), reverse=True)

    def summaries(self) -> list[dict]:
        out = []
        for item_id in self.ids():
            try:
                data = json.loads((self.dir / f"{item_id}.json").read_text())
            except (OSError, ValueError):
                continue
            out.append(self.summary(data))
        return out

    def prune(self) -> None:
        for item_id in self.ids()[self.keep :]:
            (self.dir / f"{item_id}.json").unlink(missing_ok=True)


def _run_summary(data: dict[str, Any]) -> dict[str, Any]:
    data["leftovers"] = sum(len(s.get("leftovers", [])) for s in data.pop("suites", []))
    return data


class Store(JsonStore[Run]):
    """The test runs."""

    def __init__(self, directory: Path, keep: int = 100):
        super().__init__(directory, Run, _run_summary, keep)

    def recover(self) -> None:
        """Runs that were going when the tester stopped are marked as interrupted."""
        for run_id in self.ids():
            run = self.load(run_id)
            if run and run.status not in FINISHED:
                run.status = "interrupted"
                run.finished_at = run.finished_at or now()
                run.error = "the tester stopped during the run; its scratch objects may be left over"
                self.save(run)
