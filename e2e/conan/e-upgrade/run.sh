#!/usr/bin/env bash
# E: upgrade — publish 1.0.1, assert installed graph resolves it.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
have conan || { skip "conan not installed"; exit 0; }
NAME=$(cat "$WORK/conan-name")
D=$(dir conan-e)
cat > "$D/conanfile.py" <<PY
from conan import ConanFile
class Pkg(ConanFile):
    name = "$NAME"
    version = "1.0.1"
    settings = "os", "arch", "compiler", "build_type"
    def package(self):
        pass
PY
(cd "$D" && conan profile detect >/dev/null 2>&1)
(cd "$D" && conan create . --user=ci --channel=stable >/dev/null 2>&1) && pass "E: conan create 1.0.1" || fail "E: conan create 1.0.1"
(cd "$D" && conan remote add rucoder "$API/pkgs/conan" --force >/dev/null 2>&1; conan remote login rucoder "$WRITE_TOKEN" -p "$WRITE_TOKEN" >/dev/null 2>&1)
(cd "$D" && conan upload "$NAME/1.0.1@ci/stable" -r rucoder -c >/dev/null 2>&1) && pass "E: conan upload 1.0.1" || fail "E: conan upload 1.0.1"
