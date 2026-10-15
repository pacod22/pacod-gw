"""The agents' latency histograms, added up and read.

The buckets are integer log-linear over microseconds (`crates/pacod-gw-load/src/histogram.rs`):
a value below 128 is its own bucket; above, each power of two is split into 64 buckets. Integer
arithmetic only, so that both sides agree on every bucket; `tests/test_load_histogram.py`
checks the vectors both are tested against.
"""

import math
from dataclasses import dataclass, field

LINEAR = 128
PER_OCTAVE = 64
_U64_MAX = 2**64 - 1


def index(us: int) -> int:
    """The bucket of a value in microseconds (the agent does this; here for the tests)."""
    if us < LINEAR:
        return us
    shift = us.bit_length() - 1 - 6
    mantissa = us >> shift
    return LINEAR + (shift - 1) * PER_OCTAVE + (mantissa - PER_OCTAVE)


def bounds(i: int) -> tuple[int, int]:
    """The smallest and largest value of a bucket, both included."""
    if i < LINEAR:
        return i, i
    k = i - LINEAR
    shift = k // PER_OCTAVE + 1
    mantissa = k % PER_OCTAVE + PER_OCTAVE
    return min(mantissa << shift, _U64_MAX), min(((mantissa + 1) << shift) - 1, _U64_MAX)


def value(i: int) -> float:
    lo, hi = bounds(i)
    return (lo + hi) / 2


@dataclass
class Histogram:
    n: int = 0
    min: int = 0
    max: int = 0
    sum: int = 0
    b: dict[int, int] = field(default_factory=dict)

    @classmethod
    def of(cls, wire: dict) -> "Histogram":
        """From the agents' sparse form, `{n, min, max, sum, b: [[index, count], …]}`."""
        h = cls(n=int(wire.get("n", 0)), min=int(wire.get("min", 0)), max=int(wire.get("max", 0)))
        h.sum = int(wire.get("sum", 0))
        for i, c in wire.get("b", []):
            h.b[int(i)] = h.b.get(int(i), 0) + int(c)
        return h

    def merge(self, other: "Histogram") -> None:
        if other.n == 0:
            return
        if self.n == 0:
            self.min, self.max = other.min, other.max
        else:
            self.min, self.max = min(self.min, other.min), max(self.max, other.max)
        self.n += other.n
        self.sum = min(self.sum + other.sum, _U64_MAX)
        for i, c in other.b.items():
            self.b[i] = self.b.get(i, 0) + c

    def percentile(self, q: float) -> float:
        """Nearest rank on the buckets' midpoints, within the exact smallest and largest value,
        in microseconds; 0 when empty. The same rule as the agent's."""
        if self.n == 0:
            return 0.0
        rank = min(max(math.ceil(q * self.n), 1), self.n)
        seen = 0
        for i in sorted(self.b):
            seen += self.b[i]
            if seen >= rank:
                return min(max(value(i), float(self.min)), float(self.max))
        return float(self.max)

    def mean(self) -> float:
        return self.sum / self.n if self.n else 0.0
