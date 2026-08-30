#!/usr/bin/env bash
# C: consume the package pushed in B.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
have dart || { skip "dart not installed"; exit 0; }
NAME=$(cat "$WORK/pub-name")
D=$(dir pub-c)
mkdir -p "$D/.pub-cache" "$D/lib"
cat > "$D/pubspec.yaml" <<PY
name: pub_c
environment:
  sdk: '>=3.0.0 <4.0.0'
dependencies:
  $NAME: '1.0.0'
PY
export PUB_CACHE="$D/.pub-cache"
(cd "$D" && PUB_HOSTED_URL="$API/pkgs/pub" dart pub get >/dev/null 2>&1) && pass "C: dart pub get $NAME (just pushed)" || fail "C: dart pub get $NAME"
