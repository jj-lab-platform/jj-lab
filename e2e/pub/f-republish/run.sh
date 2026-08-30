#!/usr/bin/env bash
# F: republish — pub version metadata `latest` and archive must reflect the
# re-published bytes (replace, not append).
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
have dart || { skip "dart not installed"; exit 0; }
NAME=$(cat "$WORK/pub-name")
D=$(dir pub-f)

# Build two archives for version 1.0.0 with different content.
for N in 1 2; do
  mkdir -p "$D/v$N/lib"
  cat > "$D/v$N/pubspec.yaml" <<PY
name: $NAME
version: 1.0.0
environment:
  sdk: '>=3.0.0 <4.0.0'
PY
  echo "String hello() => 'v$N';" > "$D/v$N/lib/$NAME.dart"
  tar -czf "$D/v$N/src.tar.gz" -C "$D/v$N" pubspec.yaml lib
done

curl_api -s -o /dev/null -X POST "$API/pkgs/pub/api/packages/versions/newUpload?name=$NAME&version=1.0.0" --data-binary "@$D/v1/src.tar.gz"
code=$(curl_api -s -o /dev/null -w '%{http_code}' -X POST "$API/pkgs/pub/api/packages/versions/newUpload?name=$NAME&version=1.0.0" --data-binary "@$D/v2/src.tar.gz")
assert_eq "F: pub republish 1.0.0" 201 "$code"

curl_api -s "$API/pkgs/pub/packages/$NAME/1.0.0.tar.gz" -o "$D/dl.tar.gz"
tar -xzf "$D/dl.tar.gz" -C "$D" lib 2>/dev/null
grep -q "v2" "$D/lib/$NAME.dart" && pass "F: pub archive overwritten" || fail "F: pub archive overwritten"