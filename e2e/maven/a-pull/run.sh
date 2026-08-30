#!/usr/bin/env bash
# A: pull public JVM deps (Java + Kotlin + Scala + Groovy) through pull-through.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
have gradle || { skip "gradle not installed"; exit 0; }

# Java
D=$(dir mvn-a)
mkdir -p "$D/src/main/java"
cat > "$D/build.gradle" <<BG
apply plugin: 'java'
repositories { maven { url '$API/pkgs/maven/'; allowInsecureProtocol = true } }
dependencies { implementation 'org.apache.commons:commons-lang3:3.17.0' }
BG
echo "org.gradle.daemon=false" > "$D/gradle.properties"
(cd "$D" && GRADLE_USER_HOME="$D/.guh" gradle -q dependencies --configuration runtimeClasspath >/tmp/mja.out 2>&1; grep -q commons-lang3 /tmp/mja.out) \
  && pass "A(java): gradle resolve commons-lang3" || fail "A(java): commons-lang3"

# Kotlin
D=$(dir mvn-a-kotlin)
mkdir -p "$D/src/main/kotlin"
cat > "$D/build.gradle" <<BG
plugins { id 'org.jetbrains.kotlin.jvm' version '2.4.10' }
repositories { maven { url '$API/pkgs/maven/'; allowInsecureProtocol = true } }
dependencies { implementation 'org.jetbrains.kotlin:kotlin-stdlib:2.4.10' }
BG
echo "org.gradle.daemon=false" > "$D/gradle.properties"
(cd "$D" && GRADLE_USER_HOME="$D/.guh" gradle -q dependencies --configuration implementation >/tmp/mjk.out 2>&1; grep -q kotlin-stdlib /tmp/mjk.out) \
  && pass "A(kotlin): gradle resolve kotlin-stdlib" || fail "A(kotlin): kotlin-stdlib"

# Scala
D=$(dir mvn-a-scala)
mkdir -p "$D/src/main/scala"
cat > "$D/build.gradle" <<BG
apply plugin: 'scala'
repositories { maven { url '$API/pkgs/maven/'; allowInsecureProtocol = true } }
dependencies { implementation 'org.scala-lang:scala-library:2.13.14' }
BG
echo "org.gradle.daemon=false" > "$D/gradle.properties"
(cd "$D" && GRADLE_USER_HOME="$D/.guh" gradle -q dependencies --configuration implementation >/tmp/mjs.out 2>&1; grep -q scala-library /tmp/mjs.out) \
  && pass "A(scala): gradle resolve scala-library" || fail "A(scala): scala-library"

# Groovy
D=$(dir mvn-a-groovy)
mkdir -p "$D/src/main/groovy"
cat > "$D/build.gradle" <<BG
apply plugin: 'groovy'
repositories { maven { url '$API/pkgs/maven/'; allowInsecureProtocol = true } }
dependencies { implementation 'org.apache.groovy:groovy:4.0.22' }
BG
echo "org.gradle.daemon=false" > "$D/gradle.properties"
(cd "$D" && GRADLE_USER_HOME="$D/.guh" gradle -q dependencies --configuration implementation >/tmp/mjg.out 2>&1; grep -q groovy /tmp/mjg.out) \
  && pass "A(groovy): gradle resolve groovy" || fail "A(groovy): groovy"
