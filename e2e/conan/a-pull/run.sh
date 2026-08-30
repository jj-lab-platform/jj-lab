#!/usr/bin/env bash
# A: pull a public recipe+package through pull-through.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
have conan || { skip "conan not installed"; exit 0; }
D=$(dir conan-a)
cat > "$D/conanfile.txt" <<CT
[requires]
zlib/1.3.1
[generators]
CMakeDeps
CT
(cd "$D" && conan profile detect >/dev/null 2>&1)
(cd "$D" && conan remote add rucoder "$API/pkgs/conan" --force >/dev/null 2>&1)
(cd "$D" && conan install . -r rucoder --build=missing >/dev/null 2>&1) && pass "A: conan install zlib via pull-through" || fail "A: conan install zlib"
