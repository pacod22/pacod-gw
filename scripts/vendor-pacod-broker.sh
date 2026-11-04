#!/usr/bin/env bash
# Copies a pinned commit of pacod-broker's client crates (the data model, the index's query type
# and the client) into vendor/pacod-broker/. gw-broker (crates/pacod-gw-broker) talks to the
# broker through them. As with pacod-id, builds and Docker images then need nothing outside this
# repository: deny.toml refuses git sources, and the Docker contexts see only this repository.
#
#   scripts/vendor-pacod-broker.sh [<pacod-broker checkout>] [<commit>]
#
# Defaults: the checkout next to this repository (../pacod-broker) and its HEAD. Only committed
# files are taken (git archive). vendor/pacod-broker/VERSION records the commit. The broker keeps
# these crates' manifests self-contained (its crates/pacod-broker/tests/vendorable.rs).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SRC="${1:-$(dirname "$ROOT")/pacod-broker}"
REV="${2:-HEAD}"
CRATES=(pacod-broker-model pacod-broker-index pacod-broker-client)

COMMIT="$(git -C "$SRC" rev-parse --verify "${REV}^{commit}")"
if [ -n "$(git -C "$SRC" status --porcelain)" ]; then
  echo "note: $SRC has uncommitted changes; vendoring commit $COMMIT as committed" >&2
fi

DEST="$ROOT/vendor/pacod-broker"
rm -rf "$DEST"
mkdir -p "$DEST/crates"
paths=()
for c in "${CRATES[@]}"; do paths+=("crates/$c"); done
git -C "$SRC" archive "$COMMIT" LICENSE "${paths[@]}" | tar -x -C "$DEST"
# The client's command line and the index's benchmark are not needed here.
rm -rf "$DEST/crates/pacod-broker-client/src/bin" "$DEST/crates/pacod-broker-index/benches"
sed -i -e '/^\[\[bin\]\]/,/^$/d' "$DEST/crates/pacod-broker-client/Cargo.toml"
sed -i -e '/^\[\[bench\]\]/,$d' -e '/^\[dev-dependencies\]/,/^$/d' "$DEST/crates/pacod-broker-index/Cargo.toml"
cat >"$DEST/Cargo.toml" <<'TOML'
# pacod-broker's client crates, vendored by scripts/vendor-pacod-broker.sh. Never edit them here:
# change pacod-broker and vendor again. Each crate states its versions itself.
[workspace]
resolver = "3"
members = ["crates/*"]
TOML
{
  echo "commit $COMMIT"
  echo "date $(git -C "$SRC" show -s --format=%cI "$COMMIT")"
} >"$DEST/VERSION"

echo "vendored pacod-broker $COMMIT: ${CRATES[*]}"
