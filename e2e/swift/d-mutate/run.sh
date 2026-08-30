#!/usr/bin/env bash
# D: swift releases are immutable per SE-0321 — DELETE must 405.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
code=$(curl_api -s -o /dev/null -w '%{http_code}' -X DELETE "$API/pkgs/swift/xcliorg/xclikit/1.0.0")
assert_status_in "D: swift DELETE immutable (405)" "$code" "405"
