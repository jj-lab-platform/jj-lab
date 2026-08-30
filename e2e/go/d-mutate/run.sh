#!/usr/bin/env bash
# D: go module proxy is immutable — assert DELETE yields 405.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
MOD=$(cat "$WORK/go-mod-name")
code=$(curl_api -s -o /dev/null -w '%{http_code}' -X DELETE "$API/pkgs/go/$MOD/@v/v1.0.0.zip")
assert_status_in "D: go DELETE immutable (405)" "$code" "405 404"
