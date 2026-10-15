"""One test at a time: a functional run and a load test exclude each other.

A load test changes what a functional run measures (limits, latency, the circuit breaker), and a
functional run's calls show up in a load test's figures. Messages are single calls and are not
held back.
"""


class Busy(Exception):
    def __init__(self, kind: str, run_id: str):
        what = "load test" if kind == "load" else "run"
        super().__init__(f"{what} {run_id} is still going; wait for it or cancel it")
        self.kind = kind
        self.run_id = run_id


class Exclusive:
    """Who holds the tester. Taken before the first `await` of a start, so two starts cannot
    both pass the check."""

    def __init__(self) -> None:
        self.holder: tuple[str, str] | None = None

    def take(self, kind: str, run_id: str) -> None:
        if self.holder is not None:
            raise Busy(*self.holder)
        self.holder = (kind, run_id)

    def release(self, kind: str, run_id: str) -> None:
        if self.holder == (kind, run_id):
            self.holder = None
