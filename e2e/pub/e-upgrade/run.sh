#!/usr/bin/env bash
# E: upgrade — publish 1.0.1, assert metadata latest moves.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
have dart || { skip "dart not installed"; exit 0; }
NAME=$(cat "$WORK/pub-name")
D=$(dir pub-e)
mkdir -p "$D/lib"
cat > "$D/pubspec.yaml" <<PY
name: $NAME
version: 1.0.1
environment:
  sdk: '>=3.0.0 <4.0.0'
PY
echo "String hello() => 'hello2';" > "$D/lib/$NAME.dart"
tar -czf "$D/src.tar.gz" -C "$D" pubspec.yaml lib
code=$(curl_api -s -o /dev/null -w '%{http_code}' -X POST "$API/pkgs/pub/api/packages/versions/newUpload?name=$NAME&version=1.0.1" --data-binary "@$D/src.tar.gz")
assert_eq "E: pub newUpload 1.0.1" 201 "$code"
out=$(curl_api "$API/pkgs/pub/api/packages/$NAME" -H 'Accept: application/vnd.pub.v2+json')
echo "$out" | python3 -c "import json,sys; d=json.load(sys.stdin); print(d.get('latest',{}).get('version'))" | grep -q '^1.0.1$' \
  && pass "E: latest == 1.0.1" || fail "E: latest == 1.0.1"

# Cross-version semantic ordering.
for V in 1.0.9 1.0.10 10.0.0; do
  mkdir -p "$D/v$V/lib"
  printf 'name: %s\nversion: %s\nenvironment:\n  sdk: ">=3.0.0 <4.0.0"\n' "$NAME" "$V" > "$D/v$V/pubspec.yaml"
  echo "String h() => 'x';" > "$D/v$V/lib/$NAME.dart"
  tar -czf "$D/v$V/src.tar.gz" -C "$D/v$V" pubspec.yaml lib
  curl_api -s -o /dev/null -X POST "$API/pkgs/pub/api/packages/versions/newUpload?name=$NAME&version=$V" --data-binary "@$D/v$V/src.tar.gz"
done
out=$(curl_api "$API/pkgs/pub/api/packages/$NAME" -H 'Accept: application/vnd.pub.v2+json')
echo "$out" | python3 -c "import json,sys; d=json.load(sys.stdin); print(d.get('latest',{}).get('version'))" | grep -q '^10.0.0$' \
  && pass "E: latest == 10.0.0 (semantic order)" || fail "E: latest == 10.0.0 (semantic order)"
