#!/usr/bin/env bash
# B: publish a self-made package.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
have dart || { skip "dart not installed"; exit 0; }
NAME="e2e_pub_${RUN_ID}"
echo "$NAME" > "$WORK/pub-name"
D=$(dir pub-b)
mkdir -p "$D/lib"
cat > "$D/pubspec.yaml" <<PY
name: $NAME
version: 1.0.0
environment:
  sdk: '>=3.0.0 <4.0.0'
PY
echo "String hello() => 'hello';" > "$D/lib/$NAME.dart"
tar -czf "$D/src.tar.gz" -C "$D" pubspec.yaml lib
code=$(curl_api -s -o /dev/null -w '%{http_code}' -X POST "$API/pkgs/pub/api/packages/versions/newUpload?name=$NAME&version=1.0.0" --data-binary "@$D/src.tar.gz")
assert_eq "B: pub newUpload $NAME" 201 "$code"
