#!/usr/bin/env bash
# D: mutate — delete a release files via the Warehouse-legacy API.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
NAME=$(cat "$WORK/py-pkg-name")
code=$(curl_api -s -o /dev/null -w '%{http_code}' -X DELETE "$API/pkgs/pypi/api/projects/$NAME/releases/1.0.0")
assert_eq "D: pypi delete release $NAME" 200 "$code"
code=$(curl_api -s -o /dev/null -w '%{http_code}' -X DELETE "$API/pkgs/pypi/api/projects/$NAME")
assert_eq "D: pypi delete project $NAME" 200 "$code"
