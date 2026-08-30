#!/usr/bin/env bash
# B: publish a self-made JVM library (Java) to the maven repository.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
have gradle || { skip "gradle not installed"; exit 0; }
ART="e2e-lib-$RUN_ID"
echo "$ART" > "$WORK/mvn-art-name"
D=$(dir mvn-b)
mkdir -p "$D/src/main/java/com/e2e"
cat > "$D/src/main/java/com/e2e/Lib.java" <<'JAVA'
package com.e2e;
public class Lib { public static String hello() { return "hello"; } }
JAVA
cat > "$D/build.gradle" <<BG
apply plugin: 'java'
group = 'com.e2e'
version = '1.0.0'
BG
echo "org.gradle.daemon=false" > "$D/gradle.properties"
(cd "$D" && GRADLE_USER_HOME="$D/.guh" gradle -q jar >/dev/null 2>&1) && pass "B: gradle jar" || fail "B: gradle jar"
JAR="$D/build/libs/$(basename $D)-1.0.0.jar"
[ -f "$JAR" ] || JAR=$(ls "$D/build/libs/"*.jar 2>/dev/null | head -1)
code=$(curl_api -s -o /dev/null -w '%{http_code}' -X PUT "$API/pkgs/maven/com/e2e/$ART/1.0.0/$ART-1.0.0.jar" --data-binary @"$JAR")
assert_eq "B: maven push jar $ART" 201 "$code"
printf '<project><modelVersion>4.0.0</modelVersion><groupId>com.e2e</groupId><artifactId>%s</artifactId><version>1.0.0</version></project>' "$ART" > /tmp/pom.tmp
code=$(curl_api -s -o /dev/null -w '%{http_code}' -X PUT "$API/pkgs/maven/com/e2e/$ART/1.0.0/$ART-1.0.0.pom" --data-binary @/tmp/pom.tmp)
assert_eq "B: maven push pom $ART" 201 "$code"
