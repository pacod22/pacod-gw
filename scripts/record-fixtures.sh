#!/usr/bin/env bash
# Records provider answers as fixtures: a fixed set of small calls per provider, made directly
# (not through the gateway) with the keys from .env (or from the environment). The answers are
# scrubbed and written to crates/pacod-gw-testkit/fixtures/<provider>/. Arguments are passed to
# `gw-smoke record`, for example `--provider deepseek`.
#
# The keys are exported to the recording tool only. Nothing in this script prints them, and the
# tool refuses to write a file that contains one.
set -euo pipefail
cd "$(dirname "$0")/.."

env_file="${ENV_FILE:-.env}"
read_key() {
  [[ -f "$env_file" ]] || return 0
  awk -F= -v k="$1" '$1==k{sub(/^[^=]*=/,""); print; exit}' "$env_file" | tr -d "\"'\r"
}
export GEMINI_API_KEY="${GEMINI_API_KEY:-$(read_key GEMINI_API_KEY)}"
export DEEPSEEK_API_KEY="${DEEPSEEK_API_KEY:-$(read_key DEEPSEEK_API_KEY)}"
export ZAI_API_KEY="${ZAI_API_KEY:-$(read_key ZAI_API_KEY)}"
# Vertex AI: the project, its location and the path of a service-account key file.
export VERTEX_PROJECT="${VERTEX_PROJECT:-$(read_key VERTEX_PROJECT)}"
export VERTEX_LOCATION="${VERTEX_LOCATION:-$(read_key VERTEX_LOCATION)}"
export VERTEX_CREDENTIALS_FILE="${VERTEX_CREDENTIALS_FILE:-$(read_key VERTEX_CREDENTIALS_FILE)}"
# The other keys too, so that a fixture can be checked against every key there is.
export OPENAI_API_KEY="${OPENAI_API_KEY:-$(read_key OPENAI_API_KEY)}"
export ANTHROPIC_API_KEY="${ANTHROPIC_API_KEY:-$(read_key ANTHROPIC_API_KEY)}"

cargo run -q -p pacod-gw-testkit --bin gw-smoke -- record "$@"
