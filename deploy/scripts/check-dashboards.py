#!/usr/bin/env python3
"""Static checks for the Grafana dashboards and the Prometheus alert rules.

    deploy/scripts/check-dashboards.py [--emit-rules FILE]

Checks, without Grafana or Prometheus running:
  - every dashboard is valid JSON with a unique uid and title, and every panel has a title,
    a description, a data source and at least one query;
  - every PromQL expression, in the dashboards and in the alert rules, uses only the
    metrics the core really exports. The names are read from the core's source,
    crates/pacod-gw-core/src/telemetry/metrics.rs, so a metric that is renamed or removed
    there fails this check until the dashboards and rules follow.

With --emit-rules the dashboard expressions are also written to FILE as recording rules, with
Grafana's variables replaced by plain values, so that "promtool check rules FILE" verifies
their syntax in one run.

Standard library only. Exit status 0 when everything passes.
"""

from __future__ import annotations

import argparse
import json
import pathlib
import re
import sys

DEPLOY = pathlib.Path(__file__).resolve().parent.parent
DASHBOARDS = DEPLOY / "grafana" / "dashboards"
RULES = DEPLOY / "prometheus" / "rules"
# The source of truth for metric names.
METRICS_SOURCE = DEPLOY.parent / "crates" / "pacod-gw-core" / "src" / "telemetry" / "metrics.rs"


def exported_metrics() -> tuple[set[str], set[str]]:
    """(counters and gauges, histograms) as the core's source names them."""
    text = METRICS_SOURCE.read_text()
    names = set(re.findall(r'"(gw_[a-z0-9_]+)"', text))
    histograms = set(re.findall(r'histogram!\(\s*"(gw_[a-z0-9_]+)"', text))
    return names - histograms, histograms

KEYWORDS = {"and", "or", "unless", "bool", "offset", "inf", "nan", "by", "without", "on", "ignoring",
            "group_left", "group_right", "atan2", "start", "end"}

GRAFANA_VARIABLES = {"$__rate_interval": "5m", "$__range": "1h", "$__interval": "1m"}


def substitute(expr: str) -> str:
    """Replaces Grafana variables so that the expression is plain PromQL."""
    for name, value in GRAFANA_VARIABLES.items():
        expr = expr.replace(name, value)
    return re.sub(r"\$\{?[A-Za-z_][A-Za-z0-9_]*\}?", ".*", expr)


def metric_names(expr: str) -> set[str]:
    """The metric names an expression selects, found without a full PromQL parser."""
    text = substitute(expr)
    text = re.sub(r'"(?:[^"\\]|\\.)*"', '""', text)  # string literals
    text = re.sub(r"\{[^{}]*\}", " ", text)  # label matchers
    text = re.sub(r"\[[^\[\]]*\]", " ", text)  # ranges and subqueries
    text = re.sub(r"\b(by|without|on|ignoring|group_left|group_right)\s*\([^()]*\)", " ", text)  # label lists
    names = set()
    for match in re.finditer(r"[A-Za-z_:][A-Za-z0-9_:]*", text):
        name = match.group(0)
        before = text[: match.start()]
        if re.search(r"[0-9.]$", before):  # the unit or exponent of a number: 5m, 1e3
            continue
        if name in KEYWORDS:
            continue
        if re.match(r"\s*\(", text[match.end():]):  # a function or an aggregation
            continue
        names.add(name)
    return names


def variable_selector(query: str) -> str | None:
    """label_values(<selector>, <label>) -> <selector>."""
    match = re.fullmatch(r"\s*label_values\((.*),\s*[A-Za-z_][A-Za-z0-9_]*\s*\)\s*", query, re.S)
    return match.group(1) if match else None


def walk_panels(panels):
    for panel in panels:
        yield panel
        yield from walk_panels(panel.get("panels", []))


def check_dashboards(problems: list[str]) -> list[tuple[str, str]]:
    """Returns (where, expression) for every dashboard expression."""
    expressions = []
    uids, titles = {}, {}
    files = sorted(DASHBOARDS.glob("*.json"))
    if not files:
        problems.append(f"no dashboards in {DASHBOARDS}")
    for path in files:
        try:
            board = json.loads(path.read_text())
        except json.JSONDecodeError as error:
            problems.append(f"{path.name}: not valid JSON: {error}")
            continue
        for key, seen in (("uid", uids), ("title", titles)):
            value = board.get(key)
            if not value:
                problems.append(f"{path.name}: no {key}")
            elif value in seen:
                problems.append(f"{path.name}: {key} \"{value}\" is also used by {seen[value]}")
            else:
                seen[value] = path.name

        for variable in board.get("templating", {}).get("list", []):
            if variable.get("type") != "query":
                continue
            query = variable.get("query")
            query = query.get("query") if isinstance(query, dict) else query
            selector = variable_selector(query or "")
            if selector is None:
                problems.append(f"{path.name}: variable {variable.get('name')}: expected label_values(metric, label)")
            else:
                expressions.append((f"{path.name}: variable {variable.get('name')}", selector))

        panel_count = 0
        for panel in walk_panels(board.get("panels", [])):
            if panel.get("type") == "row":
                continue
            panel_count += 1
            where = f"{path.name}: panel \"{panel.get('title', '?')}\""
            for key in ("title", "description", "datasource"):
                if not panel.get(key):
                    problems.append(f"{where}: no {key}")
            targets = panel.get("targets", [])
            if not targets:
                problems.append(f"{where}: no query")
            for target in targets:
                if not target.get("expr"):
                    problems.append(f"{where}: query {target.get('refId')} has no expression")
                else:
                    expressions.append((where, target["expr"]))
        if panel_count == 0:
            problems.append(f"{path.name}: no panels")
    return expressions


def rule_expressions() -> list[tuple[str, str]]:
    """The expr fields of the rule files, read without a YAML library."""
    expressions = []
    for path in sorted(RULES.glob("*.yml")):
        lines = path.read_text().splitlines()
        index = 0
        while index < len(lines):
            match = re.match(r"^(\s*)expr:\s*(.*)$", lines[index])
            index += 1
            if not match:
                continue
            indent, rest = len(match.group(1)), match.group(2).strip()
            if rest not in ("|", "|-", ">", ">-"):
                expressions.append((f"{path.name}: line {index}", rest))
                continue
            block = []
            while index < len(lines) and (not lines[index].strip() or len(lines[index]) - len(lines[index].lstrip()) > indent):
                block.append(lines[index].strip())
                index += 1
            expressions.append((f"{path.name}: line {index - len(block)}", " ".join(block)))
    return expressions


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--emit-rules", metavar="FILE", help="write the dashboard expressions as recording rules")
    args = parser.parse_args()

    problems: list[str] = []
    try:
        counters_and_gauges, histograms = exported_metrics()
    except OSError as error:
        print(f"FAIL  cannot read the core's metric names: {error}", file=sys.stderr)
        return 1
    if not counters_and_gauges or not histograms:
        print(f"FAIL  no metric names found in {METRICS_SOURCE}", file=sys.stderr)
        return 1
    metrics = counters_and_gauges | {f"{name}{suffix}" for name in histograms for suffix in ("_bucket", "_sum", "_count")}

    dashboard_expressions = check_dashboards(problems)
    rules = rule_expressions()
    if not rules:
        problems.append(f"no alert rule expressions found in {RULES}")

    used = set()
    for where, expr in dashboard_expressions + rules:
        names = metric_names(expr)
        if not names:
            problems.append(f"{where}: no metric found in: {expr}")
        for name in sorted(names - metrics):
            hint = " (a histogram: use _bucket, _sum or _count)" if name in histograms else ""
            problems.append(f"{where}: \"{name}\" is not exported by the core{hint}")
        used |= names & metrics

    if args.emit_rules:
        lines = ["groups:", "  - name: dashboard-expressions", "    rules:"]
        for number, (where, expr) in enumerate(dashboard_expressions):
            lines.append(f"      # {where}")
            lines.append(f"      - record: dashboard:expression_{number}")
            lines.append(f"        expr: {json.dumps(substitute(expr))}")
        pathlib.Path(args.emit_rules).write_text("\n".join(lines) + "\n")

    for problem in problems:
        print(f"FAIL  {problem}", file=sys.stderr)
    unused = sorted(name for name in counters_and_gauges | histograms
                    if not any(metric == name or metric.startswith(name + "_") for metric in used))
    print(f"{len(counters_and_gauges) + len(histograms)} metrics exported by the core; "
          f"{len(dashboard_expressions)} dashboard expressions and {len(rules)} rule expressions checked; "
          f"{len(problems)} problem(s)")
    if unused:
        print(f"note: not shown on any dashboard or rule: {', '.join(unused)}")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
