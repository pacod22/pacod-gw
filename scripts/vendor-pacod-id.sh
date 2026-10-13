#!/usr/bin/env bash
# Copies a pinned commit of pacod-id (the identity service and its libraries) into
# vendor/pacod-id/, and packs its npm packages into ui/vendor/. Builds, Docker images and CI then
# need nothing outside this repository: deny.toml refuses git sources, and the Docker contexts
# see only this repository. Once pacod-id is published, crates and packages come from their
# registries and the id image by digest instead.
#
#   scripts/vendor-pacod-id.sh [<pacod-id checkout>] [<commit>]
#
# Defaults: the checkout next to this repository (../pacod-id) and its HEAD. Only committed
# files are taken (git archive). vendor/pacod-id/VERSION records the commit.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SRC="${1:-$(dirname "$ROOT")/pacod-id}"
REV="${2:-HEAD}"
# The npm packages the console uses.
PACKAGES=(authz ui client sveltekit)

COMMIT="$(git -C "$SRC" rev-parse --verify "${REV}^{commit}")"
if [ -n "$(git -C "$SRC" status --porcelain)" ]; then
  echo "note: $SRC has uncommitted changes; vendoring commit $COMMIT as committed" >&2
fi

DEST="$ROOT/vendor/pacod-id"
rm -rf "$DEST"
mkdir -p "$DEST"
git -C "$SRC" archive "$COMMIT" | tar -x -C "$DEST"
{
  echo "commit $COMMIT"
  echo "date $(git -C "$SRC" show -s --format=%cI "$COMMIT")"
} >"$DEST/VERSION"

# The npm packages are built from a second copy of the same commit and packed.
BUILD="$(mktemp -d)"
trap 'rm -rf "$BUILD"' EXIT
git -C "$SRC" archive "$COMMIT" | tar -x -C "$BUILD"
(
  cd "$BUILD"
  npm ci --no-audit --no-fund >/dev/null
  for p in "${PACKAGES[@]}"; do npm run build -w "packages/$p" >/dev/null; done
)
mkdir -p "$ROOT/ui/vendor"
rm -f "$ROOT"/ui/vendor/pacod-*.tgz
for p in "${PACKAGES[@]}"; do
  (cd "$BUILD/packages/$p" && npm pack --pack-destination "$ROOT/ui/vendor" >/dev/null)
done

echo "vendored pacod-id $COMMIT"
echo "when its Helm chart changed: make helm-deps (deploy-check compares the two)"
ls "$ROOT/ui/vendor"
