#!/usr/bin/env bash
# A: generic has no central upstream; verify endpoint serves 200 (empty store 404 acceptable).
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
code=$(curl_api -s -o /dev/null -w '%{http_code}' "$API/pkgs/generic/nonexist/1.0/f")
case "$code" in 404|200) pass "A: generic endpoint reachable";; *) fail "A: generic endpoint ($code)";; esac
