#!/usr/bin/env bash
# Copies a pinned commit of pacod-aggregator's catalogue crate (pacod-agg-catalog) into
# vendor/pacod-aggregator/. The gateways read every source of model data with it: the
# aggregator's API, OpenRouter, LiteLLM, an uploaded file and the models file. As with
# pacod-broker, builds and Docker images then need nothing outside this repository: deny.toml
# refuses git sources, and the Docker contexts see only this repository.
#
#   scripts/vendor-pacod-aggregator.sh [<pacod-aggregator checkout>] [<commit>]
#
# Defaults: the checkout next to this repository (../pacod-aggregator) and its HEAD. Only
# committed files are taken (git archive). vendor/pacod-aggregator/VERSION records the commit.
# The aggregator keeps the crate's manifest self-contained (its tests/vendorable.rs); its
# fixtures come along, and the gateway's tests read them.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SRC="${1:-$(dirname "$ROOT")/pacod-aggregator}"
REV="${2:-HEAD}"
CRATE=pacod-agg-catalog

COMMIT="$(git -C "$SRC" rev-parse --verify "${REV}^{commit}")"
if [ -n "$(git -C "$SRC" status --porcelain)" ]; then
  echo "note: $SRC has uncommitted changes; vendoring commit $COMMIT as committed" >&2
fi

DEST="$ROOT/vendor/pacod-aggregator"
rm -rf "$DEST"
mkdir -p "$DEST/crates"
git -C "$SRC" archive "$COMMIT" LICENSE "crates/$CRATE" | tar -x -C "$DEST"
cat >"$DEST/Cargo.toml" <<'TOML'
# pacod-aggregator's catalogue crate, vendored by scripts/vendor-pacod-aggregator.sh. Never edit it
# here: change pacod-aggregator and vendor again. The crate states its versions itself.
[workspace]
resolver = "3"
members = ["crates/*"]
TOML
{
  echo "commit $COMMIT"
  echo "date $(git -C "$SRC" show -s --format=%cI "$COMMIT")"
} >"$DEST/VERSION"

echo "vendored pacod-aggregator $COMMIT: $CRATE"
