#!/usr/bin/env bash
# D: mutate — DELETE the artifact package, assert 404.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
NAME=$(cat "$WORK/generic-name")
code=$(curl_api -s -o /dev/null -w '%{http_code}' -X DELETE "$API/pkgs/generic/$NAME.version")
assert_eq "D: generic delete package" 200 "$code"
code=$(curl_api -s -o /dev/null -w '%{http_code}' "$API/pkgs/generic/$NAME/1.0.0/artifact.txt")
assert_status_in "D: generic gone" "$code" "404"
