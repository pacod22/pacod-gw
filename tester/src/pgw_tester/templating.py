"""`{{name}}` variables in step values, and dotted paths into JSON."""

import re
from typing import Any

_VAR = re.compile(r"\{\{\s*([A-Za-z_][\w.-]*)\s*\}\}")
_MISSING = object()


class MissingVariable(Exception):
    def __init__(self, name: str):
        super().__init__(f"the variable {{{{{name}}}}} is not set")
        self.name = name


def lookup(variables: dict[str, Any], name: str) -> Any:
    """A variable; `model.openai` is looked up as `variables["model"]["openai"]` or as the flat
    name, whichever exists."""
    if name in variables:
        return variables[name]
    found, value = get_path(variables, name)
    if not found:
        raise MissingVariable(name)
    return value


def render(value: Any, variables: dict[str, Any]) -> Any:
    """The value with every variable replaced. A text that is a single variable takes the
    variable's value as it is, so that numbers and objects keep their type."""
    if isinstance(value, str):
        whole = _VAR.fullmatch(value.strip())
        if whole:
            return lookup(variables, whole[1])
        return _VAR.sub(lambda m: _text(lookup(variables, m[1])), value)
    if isinstance(value, list):
        return [render(v, variables) for v in value]
    if isinstance(value, dict):
        return {render(k, variables): render(v, variables) for k, v in value.items()}
    return value


def _text(value: Any) -> str:
    if isinstance(value, bool):
        return "true" if value else "false"
    return str(value)


def get_path(data: Any, path: str) -> tuple[bool, Any]:
    """The value at a dotted path such as `choices.0.message.content`. A `*` maps the rest of
    the path over every element of a list (`data.*.id`), and `[key=value]` takes the first
    element whose member `key` is the text `value` (`content.[type=tool_use].id`). An empty
    path is the whole value."""
    if path in ("", "."):
        return True, data
    return _walk(data, path.split("."))


def _walk(data: Any, parts: list[str]) -> tuple[bool, Any]:
    if not parts:
        return True, data
    head, rest = parts[0], parts[1:]
    if head == "*":
        if not isinstance(data, list):
            return False, None
        out = []
        for item in data:
            found, value = _walk(item, rest)
            if found:
                out.append(value)
        return True, out
    if isinstance(data, dict):
        if head not in data:
            return False, None
        return _walk(data[head], rest)
    if isinstance(data, list) and head.startswith("[") and head.endswith("]") and "=" in head:
        key, wanted = head[1:-1].split("=", 1)
        for item in data:
            if isinstance(item, dict) and str(item.get(key)) == wanted:
                return _walk(item, rest)
        return False, None
    if isinstance(data, list):
        try:
            index = int(head)
        except ValueError:
            return False, None
        if -len(data) <= index < len(data):
            return _walk(data[index], rest)
    return False, None
