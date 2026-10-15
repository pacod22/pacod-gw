"""The load split across agents: equal shares, the remainder to the first agents."""

from .contract import Stage
from .model import LoadShape, Share


def split_int(total: int, n: int) -> list[int]:
    base, extra = divmod(total, n)
    return [base + (1 if i < extra else 0) for i in range(n)]


def shares(shape: LoadShape, n: int, budget: int | None) -> list[Share]:
    """Each agent's stages, in-flight cap and calls. Concurrency targets are whole calls; rates
    are split exactly. `budget` is the calls left for the agents after the probes and the
    sanity call."""
    if shape.mode == "concurrency":
        per_stage = [split_int(round(s.target), n) for s in shape.stages]
    else:
        per_stage = [[s.target / n] * n for s in shape.stages]
    in_flight = split_int(shape.max_in_flight, n) if shape.max_in_flight is not None else [None] * n
    calls = split_int(budget, n) if budget is not None else [None] * n
    return [
        Share(
            stages=[
                Stage(duration_s=s.duration_s, target=per_stage[k][i]) for k, s in enumerate(shape.stages)
            ],
            max_in_flight=in_flight[i],
            max_calls=calls[i],
        )
        for i in range(n)
    ]
