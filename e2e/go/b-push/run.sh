#!/usr/bin/env bash
# B: publish a self-made module.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
have go || { skip "go not installed"; exit 0; }
MOD="example.com/private/lib-$RUN_ID"
V="v1.0.0"
echo "$MOD" > "$WORK/go-mod-name"
D=$(dir go-b)
ZIPROOT="$D/ziproot/$MOD@$V"
mkdir -p "$ZIPROOT"
printf 'module %s\n\ngo 1.21\n' "$MOD" > "$ZIPROOT/go.mod"
echo 'package lib
func Hello() string { return "hello" }' > "$ZIPROOT/lib.go"
(cd "$D/ziproot" && zip -qr "$D/mod.zip" "$MOD@$V" >/dev/null 2>&1)
code=$(curl_api -s -o /dev/null -w '%{http_code}' -X PUT "$API/pkgs/go/upload?module=$MOD&version=$V" --data-binary @"$D/mod.zip")
assert_eq "B: go upload $MOD" 201 "$code"
