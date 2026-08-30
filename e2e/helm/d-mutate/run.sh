#!/usr/bin/env bash
# D: mutate — DELETE the chart version, assert chart tgz 404s.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
NAME=$(cat "$WORK/helm-name")
code=$(curl_api -s -o /dev/null -w '%{http_code}' -X DELETE "$API/pkgs/helm/api/charts/$NAME/0.1.0")
assert_eq "D: helm delete version" 200 "$code"
code=$(curl_api -s -o /dev/null -w '%{http_code}' "$API/pkgs/helm/charts/$NAME-0.1.0.tgz")
assert_status_in "D: helm tgz gone" "$code" "404"
