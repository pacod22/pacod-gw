"""Runs on disk: one JSON file per run, the newest `keep` of them."""

import json
import re
from pathlib import Path

from .results import FINISHED, Run, now

RUN_ID = re.compile(r"^[0-9]{8}-[0-9]{6}-[0-9a-f]{6}$")


class Store:
    def __init__(self, directory: Path, keep: int = 100):
        self.dir = directory
        self.keep = keep
        self.dir.mkdir(parents=True, exist_ok=True)

    def _file(self, run_id: str) -> Path | None:
        return self.dir / f"{run_id}.json" if RUN_ID.match(run_id) else None

    def save(self, run: Run) -> None:
        target = self._file(run.id)
        assert target is not None
        tmp = target.with_suffix(".tmp")
        tmp.write_text(run.model_dump_json())
        tmp.replace(target)

    def load(self, run_id: str) -> Run | None:
        f = self._file(run_id)
        if f is None or not f.is_file():
            return None
        try:
            return Run.model_validate_json(f.read_text())
        except ValueError:
            return None

    def ids(self) -> list[str]:
        """Newest first: the ids start with the time."""
        return sorted((f.stem for f in self.dir.glob("*.json") if RUN_ID.match(f.stem)), reverse=True)

    def summaries(self) -> list[dict]:
        out = []
        for run_id in self.ids():
            try:
                data = json.loads((self.dir / f"{run_id}.json").read_text())
            except (OSError, ValueError):
                continue
            leftovers = sum(len(s.get("leftovers", [])) for s in data.pop("suites", []))
            data["leftovers"] = leftovers
            out.append(data)
        return out

    def prune(self) -> None:
        for run_id in self.ids()[self.keep :]:
            (self.dir / f"{run_id}.json").unlink(missing_ok=True)

    def recover(self) -> None:
        """Runs that were going when the tester stopped are marked as interrupted."""
        for run_id in self.ids():
            run = self.load(run_id)
            if run and run.status not in FINISHED:
                run.status = "interrupted"
                run.finished_at = run.finished_at or now()
                run.error = "the tester stopped during the run; its scratch objects may be left over"
                self.save(run)
