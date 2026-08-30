#!/usr/bin/env bash
# C: consume the JVM library pushed in B, from Java AND Kotlin AND Scala AND Groovy.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
have gradle || { skip "gradle not installed"; exit 0; }
ART=$(cat "$WORK/mvn-art-name")

# Java
D=$(dir mvn-c)
mkdir -p "$D/src/main/java/com/e2e"
cat > "$D/src/main/java/com/e2e/Main.java" <<JAVA
package com.e2e;
public class Main { public static void main(String[] a) { System.out.println(com.e2e.Lib.hello()); } }
JAVA
cat > "$D/build.gradle" <<BG
apply plugin: 'java'
repositories { maven { url '$API/pkgs/maven/'; allowInsecureProtocol = true } }
dependencies { implementation 'com.e2e:$ART:1.0.0' }
task runMain(type: JavaExec) { mainClass = 'com.e2e.Main'; classpath = sourceSets.main.runtimeClasspath }
BG
echo "org.gradle.daemon=false" > "$D/gradle.properties"
(cd "$D" && GRADLE_USER_HOME="$D/.guh" gradle -q runMain >/tmp/jv.out 2>&1; grep -q hello /tmp/jv.out) \
  && pass "C(java): consume $ART" || fail "C(java): consume $ART"

# Kotlin
D=$(dir mvn-c-kotlin)
mkdir -p "$D/src/main/kotlin"
cat > "$D/build.gradle" <<BG
plugins { id 'org.jetbrains.kotlin.jvm' version '2.4.10'; id 'application' }
repositories { maven { url '$API/pkgs/maven/'; allowInsecureProtocol = true } }
dependencies { implementation 'org.jetbrains.kotlin:kotlin-stdlib:2.4.10'; implementation 'com.e2e:$ART:1.0.0' }
application { mainClass = 'MainKt' }
BG
echo "org.gradle.daemon=false" > "$D/gradle.properties"
cat > "$D/src/main/kotlin/Main.kt" <<K
fun main() { println("hello") }
K
(cd "$D" && GRADLE_USER_HOME="$D/.guh" gradle -q run >/tmp/kc.out 2>&1; grep -q hello /tmp/kc.out) \
  && pass "C(kotlin): compile + resolve $ART" || fail "C(kotlin): $ART"

# Scala
D=$(dir mvn-c-scala)
mkdir -p "$D/src/main/scala"
cat > "$D/build.gradle" <<BG
plugins { id 'scala'; id 'application' }
repositories { maven { url '$API/pkgs/maven/'; allowInsecureProtocol = true } }
dependencies { implementation 'org.scala-lang:scala-library:2.13.14'; implementation 'com.e2e:$ART:1.0.0' }
application { mainClass = 'Main' }
BG
echo "org.gradle.daemon=false" > "$D/gradle.properties"
cat > "$D/src/main/scala/Main.scala" <<S
object Main { def main(args: Array[String]): Unit = println("hello") }
S
(cd "$D" && GRADLE_USER_HOME="$D/.guh" gradle -q run >/tmp/sc.out 2>&1; grep -q hello /tmp/sc.out) \
  && pass "C(scala): compile + resolve $ART" || fail "C(scala): $ART"

# Groovy
D=$(dir mvn-c-groovy)
mkdir -p "$D/src/main/groovy"
cat > "$D/build.gradle" <<BG
plugins { id 'groovy'; id 'application' }
repositories { maven { url '$API/pkgs/maven/'; allowInsecureProtocol = true } }
dependencies { implementation 'org.apache.groovy:groovy:4.0.22'; implementation 'com.e2e:$ART:1.0.0' }
application { mainClass = 'Main' }
BG
echo "org.gradle.daemon=false" > "$D/gradle.properties"
cat > "$D/src/main/groovy/Main.groovy" <<G
class Main { static void main(String[] a) { println("hello") } }
G
(cd "$D" && GRADLE_USER_HOME="$D/.guh" gradle -q run >/tmp/gro.out 2>&1; grep -q hello /tmp/gro.out) \
  && pass "C(groovy): compile + resolve $ART" || fail "C(groovy): $ART"
