"""The one request every call of a load test sends, by interface and mode.

The shapes follow the console's message composer (`ui/src/lib/message.ts`). The prompt is the
word "word" repeated, as `gw-load` sends it: the mock answers with about as many stream chunks.
"""

import json
from typing import Any

from .contract import AuthHeader, RequestSpec
from .model import LoadRequest


def auth_header(interface: str) -> AuthHeader:
    """Where the SDKs put the key: Anthropic's in x-api-key, Google's in x-goog-api-key."""
    if interface == "anthropic":
        return "x-api-key"
    if interface == "gemini":
        return "x-goog-api-key"
    return "authorization"


def build(req: LoadRequest, model: str, provider: str = "", upstream: str = "") -> RequestSpec:
    """`model` is the gateway's name for the model (managed mode); `provider` and `upstream`
    are the provider's name and its id for the model (proxy mode)."""
    prompt = " ".join(["word"] * req.prompt_words)
    name = model if req.mode == "managed" else upstream
    stream = req.stream and req.interface != "embeddings"
    headers = {"content-type": "application/json"}
    if req.interface == "anthropic":
        headers["anthropic-version"] = "2023-06-01"
    if req.target != "mock":
        # One attempt per call: a retry would make two provider calls of one, and a load test
        # against a paid provider counts calls.
        headers["x-gw-retries"] = "1"
    keyed = req.own if req.target == "own" else req.external if req.target == "external" else None
    if keyed is not None and keyed.mock_options and req.mode == "proxy":
        headers["x-mock"] = keyed.mock_options
    body = _body(req, name, prompt, stream)
    return RequestSpec(
        method="POST",
        path=_path(req, name, provider, stream),
        headers=headers,
        body=json.dumps(body, separators=(",", ":")),
        stream=stream,
    )


def _body(req: LoadRequest, name: str, prompt: str, stream: bool) -> dict[str, Any]:
    n = req.max_tokens
    match req.interface:
        case "gemini":
            return {
                "contents": [{"role": "user", "parts": [{"text": prompt}]}],
                "generationConfig": {"maxOutputTokens": n},
            }
        case "embeddings":
            return {"model": name, "input": [prompt]}
        case "anthropic":
            body: dict[str, Any] = {
                "model": name,
                "max_tokens": n,
                "messages": [{"role": "user", "content": prompt}],
            }
        case "native":
            body = {
                "model": name,
                "messages": [{"role": "user", "content": [{"type": "text", "text": prompt}]}],
                "max_output_tokens": n,
            }
        case _:
            body = {"model": name, "messages": [{"role": "user", "content": prompt}], "max_tokens": n}
    if stream:
        body["stream"] = True
        if req.interface == "openai":
            body["stream_options"] = {"include_usage": True}
    return body


def _path(req: LoadRequest, name: str, provider: str, stream: bool) -> str:
    call = "streamGenerateContent?alt=sse" if stream else "generateContent"
    below = {
        "openai": "/v1/chat/completions",
        "anthropic": "/v1/messages",
        "gemini": f"/v1beta/models/{name}:{call}",
        "native": "/gw/v1/chat",
        "embeddings": "/v1/embeddings",
    }[req.interface]
    return below if req.mode == "managed" else f"/proxy/{provider}{below}"
