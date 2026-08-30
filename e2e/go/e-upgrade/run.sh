#!/usr/bin/env bash
# E: upgrade — upload v1.1.0, assert @latest and @v/list move.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
have go || { skip "go not installed"; exit 0; }
MOD=$(cat "$WORK/go-mod-name")
V="v1.1.0"
D=$(dir go-e)
ZIPROOT="$D/ziproot/$MOD@$V"
mkdir -p "$ZIPROOT"
printf 'module %s\n\ngo 1.21\n' "$MOD" > "$ZIPROOT/go.mod"
echo 'package lib
func Hello() string { return "hello2" }' > "$ZIPROOT/lib.go"
(cd "$D/ziproot" && zip -qr "$D/mod.zip" "$MOD@$V" >/dev/null 2>&1)
code=$(curl_api -s -o /dev/null -w '%{http_code}' -X PUT "$API/pkgs/go/upload?module=$MOD&version=$V" --data-binary @"$D/mod.zip")
assert_eq "E: go upload v1.1.0" 201 "$code"
latest=$(curl_api "$API/pkgs/go/$MOD/@latest")
echo "$latest" | grep -q '"Version":"v1.1.0"' && pass "E: @latest == v1.1.0" || fail "E: @latest == v1.1.0"
curl_api "$API/pkgs/go/$MOD/@v/list" | grep -qx 'v1.1.0' && pass "E: @v/list includes v1.1.0" || fail "E: @v/list includes v1.1.0"

# Cross-version semantic ordering (Go leading-v): v1.0.9 < v1.0.10 < v2.0.
# < v10.0.0 semantically; lexically "10" sorts before "2".
for V in v1.0.9 v1.0.10 v2.0.0 v10.0.0; do
  Z=$(dir go-e2-$V)
  ZR="$Z/ziproot/$MOD@$V"
  mkdir -p "$ZR"
  printf 'module %s\n\ngo 1.21\n' "$MOD" > "$ZR/go.mod"
  echo 'package lib
func Hello() string { return "x" }' > "$ZR/lib.go"
  (cd "$Z/ziproot" && zip -qr "$Z/mod.zip" "$MOD@$V" >/dev/null 2>&1)
  curl_api -s -o /dev/null -X PUT "$API/pkgs/go/upload?module=$MOD&version=$V" --data-binary @"$Z/mod.zip"
done
latest=$(curl_api "$API/pkgs/go/$MOD/@latest")
echo "$latest" | grep -q '"Version":"v10.0.0"' && pass "E: @latest == v10.0.0 (semantic order)" || fail "E: @latest == v10.0.0 (semantic order)"
