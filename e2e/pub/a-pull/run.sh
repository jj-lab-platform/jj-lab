#!/usr/bin/env bash
# A: pull a public package.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
have dart || { skip "dart not installed"; exit 0; }
D=$(dir pub-a)
mkdir -p "$D/.pub-cache"
cat > "$D/pubspec.yaml" <<'PY'
name: pub_a
environment:
  sdk: '>=3.0.0 <4.0.0'
dependencies:
  meta: '^1.0.0'
PY
export PUB_CACHE="$D/.pub-cache"
(cd "$D" && PUB_HOSTED_URL="$API/pkgs/pub" dart pub get >/dev/null 2>&1) && pass "A: dart pub get meta via pull-through" || fail "A: dart pub get meta"
