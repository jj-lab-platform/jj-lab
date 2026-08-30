#!/usr/bin/env bash
# F: republish — same coordinate with different bytes must overwrite the pom.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
ART=$(cat "$WORK/mvn-art-name")
D=$(dir mvn-f)

printf '<project><modelVersion>4.0.0</modelVersion><groupId>com.e2e</groupId><artifactId>%s</artifactId><version>1.0.</version><description>ONE</description></project>' "$ART" > "$D/a.pom"
printf '<project><modelVersion>4.0.0</modelVersion><groupId>com.e2e</groupId><artifactId>%s</artifactId><version>1.0.</version><description>TWO</description></project>' "$ART" > "$D/b.pom"

curl_api -s -o /dev/null -X PUT "$API/pkgs/maven/com/e2e/$ART/1.0.0/$ART-1.0.0.pom" --data-binary @"$D/a.pom"
code=$(curl_api -s -o /dev/null -w '%{http_code}' -X PUT "$API/pkgs/maven/com/e2e/$ART/1.0.0/$ART-1.0.0.pom" --data-binary @"$D/b.pom")
assert_eq "F: maven republish pom" 201 "$code"

out=$(curl_api "$API/pkgs/maven/com/e2e/$ART/1.0.0/$ART-1.0.0.pom")
echo "$out" | grep -q 'TWO' && pass "F: pom overwritten" || fail "F: pom overwritten"
echo "$out" | grep -q 'ONE' && fail "F: stale bytes leaked" || pass "F: no stale bytes"

# Delete then republish should restore.
code=$(curl_api -s -o /dev/null -w '%{http_code}' -X DELETE "$API/pkgs/maven/com/e2e/$ART/1.0.0/$ART-1.0.0.pom")
assert_eq "F: maven delete pom" 200 "$code"
code=$(curl_api -s -o /dev/null -w '%{http_code}' -X PUT "$API/pkgs/maven/com/e2e/$ART/1.0.0/$ART-1.0.0.pom" --data-binary @"$D/b.pom")
assert_eq "F: maven republish after delete" 201 "$code"
out=$(curl_api "$API/pkgs/maven/com/e2e/$ART/1.0.0/$ART-1.0.0.pom")
echo "$out" | grep -q 'TWO' && pass "F: pom back after delete+republish" || fail "F: pom back after delete+republish"