#!/usr/bin/env bash
# D: mutate — pub has no upstream delete; DELETE removes the version.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
NAME=$(cat "$WORK/pub-name")
code=$(curl_api -s -o /dev/null -w '%{http_code}' -X DELETE "$API/pkgs/pub/api/packages/$NAME/versions/1.0.0")
assert_eq "D: pub retract $NAME" 200 "$code"
code=$(curl_api -s -o /dev/null -w '%{http_code}' -X PUT "$API/pkgs/pub/api/packages/$NAME/versions/1.0.0")
assert_eq "D: pub unretract" 200 "$code"
