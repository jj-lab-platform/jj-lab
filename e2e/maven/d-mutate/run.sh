#!/usr/bin/env bash
# D: mutate — DELETE a maven artifact file, assert it 404s afterward.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
ART=$(cat "$WORK/mvn-art-name")
code=$(curl_api -s -o /dev/null -w '%{http_code}' -X DELETE "$API/pkgs/maven/com/e2e/$ART/1.0.0/$ART-1.0.0.pom")
assert_eq "D: maven DELETE pom" 200 "$code"
code=$(curl_api -s -o /dev/null -w '%{http_code}' "$API/pkgs/maven/com/e2e/$ART/1.0.0/$ART-1.0.0.pom")
assert_status_in "D: maven pom gone" "$code" "404"
