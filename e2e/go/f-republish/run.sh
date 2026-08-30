#!/usr/bin/env bash
# F: republish — same module/version with different zip must overwrite.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
have go || { skip "go not installed"; exit 0; }
MOD=$(cat "$WORK/go-mod-name")
V="v1.0.0"
D=$(dir go-f)

for N in 1 2; do
  ZR="$D/z$N/$MOD@$V"
  mkdir -p "$ZR"
  printf 'module %s\n\ngo 1.21\n' "$MOD" > "$ZR/go.mod"
  echo "package lib
func Hello() string { return \"v$N\" }" > "$ZR/lib.go"
  (cd "$D/z$N" && zip -qr "$D/m$N.zip" "$MOD@$V" >/dev/null 2>&1)
done

curl_api -s -o /dev/null -X PUT "$API/pkgs/go/upload?module=$MOD&version=$V" --data-binary @"$D/m1.zip"
code=$(curl_api -s -o /dev/null -w '%{http_code}' -X PUT "$API/pkgs/go/upload?module=$MOD&version=$V" --data-binary @"$D/m2.zip")
assert_eq "F: go republish v1.0.0" 201 "$code"

curl_api -s "$API/pkgs/go/$MOD/@v/v1.0.0.zip" -o "$D/dl.zip"
unzip -p "$D/dl.zip" "$MOD@$V/lib.go" | grep -q 'v2' && pass "F: go zip overwritten" || fail "F: go zip overwritten"