#!/usr/bin/env bash
# C: consume the recipe+package pushed in B.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
have conan || { skip "conan not installed"; exit 0; }
NAME=$(cat "$WORK/conan-name")
D=$(dir conan-c)
cat > "$D/conanfile.txt" <<CT
[requires]
$NAME/1.0.0@ci/stable
[generators]
CMakeDeps
CT
(cd "$D" && conan profile detect >/dev/null 2>&1)
(cd "$D" && conan remote add rucoder "$API/pkgs/conan" --force >/dev/null 2>&1; conan remote login rucoder "$WRITE_TOKEN" -p "$WRITE_TOKEN" >/dev/null 2>&1)
# wipe local cache so install pulls from registry
conan remove "$NAME/1.0.0@ci/stable" -c >/dev/null 2>&1
(cd "$D" && conan install . -r rucoder >/dev/null 2>&1) && pass "C: conan install $NAME (just pushed)" || fail "C: conan install $NAME"
