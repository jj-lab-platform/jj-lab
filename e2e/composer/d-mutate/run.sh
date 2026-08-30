#!/usr/bin/env bash
# D: mutate — DELETE the package pushed in B, assert p2 404s.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
NAME=$(cat "$WORK/comp-name")
code=$(curl_api -s -o /dev/null -w '%{http_code}' -X DELETE "$API/pkgs/composer/api/packages/e2e-test/$NAME")
assert_eq "D: composer DELETE e2e-test/$NAME" 200 "$code"
code=$(curl_api -s -o /dev/null -w '%{http_code}' "$API/pkgs/composer/p2/e2e-test/$NAME.json")
assert_status_in "D: composer p2 gone" "$code" "404"
