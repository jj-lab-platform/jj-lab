#!/usr/bin/env bash
# E: upgrade — push 1.0.1 pom+jar, assert metadata latest moves to 1..1.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
have gradle || { skip "gradle not installed"; exit 0; }
ART=$(cat "$WORK/mvn-art-name")
printf '<project><modelVersion>4.0.0</modelVersion><groupId>com.e2e</groupId><artifactId>%s</artifactId><version>1.0.1</version></project>' "$ART" > /tmp/pom2.tmp
code=$(curl_api -s -o /dev/null -w '%{http_code}' -X PUT "$API/pkgs/maven/com/e2e/$ART/1.0.1/$ART-1.0.1.pom" --data-binary @/tmp/pom2.tmp)
assert_eq "E: maven push pom 1.0.1" 201 "$code"
code=$(curl_api -s -o /dev/null -w '%{http_code}' -X PUT "$API/pkgs/maven/com/e2e/$ART/1.0.1/$ART-1.0.1.jar" --data-binary "x")
assert_eq "E: maven push jar 1.0.1" 201 "$code"
out=$(curl_api "$API/pkgs/maven/com/e2e/$ART/maven-metadata.xml")
echo "$out" | grep -q '<latest>1.0.1</latest>' && pass "E: metadata latest == 1.0.1" || fail "E: metadata latest == 1.0.1"

# Cross-version semantic ordering: 2.0.0 < 2.0.1 < 10.0.0 semantically.
for V in 2.0.0 2.0.1 10.0.0; do
  curl_api -s -o /dev/null -X PUT "$API/pkgs/maven/com/e2e/$ART/$V/$ART-$V.pom" --data-binary "<project/>"
done
out=$(curl_api "$API/pkgs/maven/com/e2e/$ART/maven-metadata.xml")
echo "$out" | grep -q '<latest>10.0.0</latest>' && pass "E: metadata latest == 10.0.0 (semantic order)" || fail "E: metadata latest == 10.0.0 (semantic order)"
