#!/usr/bin/env bash
# E: upgrade — upload a new version, assert it downloads.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
NAME=$(cat "$WORK/generic-name")
D=$(dir generic-e)
echo "hello-generic-2" > "$D/artifact.txt"
code=$(curl_api -s -o /dev/null -w '%{http_code}' -X PUT "$API/pkgs/generic/$NAME/1.0.1/artifact.txt" --data-binary @"$D/artifact.txt")
assert_eq "E: generic upload 1.0.1" 201 "$code"
out=$(curl_api "$API/pkgs/generic/$NAME/1.0.1/artifact.txt")
[ "$out" = "hello-generic-2" ] && pass "E: generic download 1.0.1" || fail "E: generic download 1.0.1"
