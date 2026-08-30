#!/usr/bin/env bash
# D: mutate — unlist the version, then relist.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
ID=$(cat "$WORK/nuget-name")
code=$(curl_api -s -o /dev/null -w '%{http_code}' -X DELETE "$API/pkgs/nuget/v3/package/$ID/1.0.0")
assert_status_in "D: nuget unlist" "$code" "204 200 404"
code=$(curl_api -s -o /dev/null -w '%{http_code}' -X PUT "$API/pkgs/nuget/api/v2/package/$ID/1.0.0")
assert_status_in "D: nuget relist" "$code" "200 404"
