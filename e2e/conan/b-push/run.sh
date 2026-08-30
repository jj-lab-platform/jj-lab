#!/usr/bin/env bash
# B: publish a self-made recipe + binary package.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
have conan || { skip "conan not installed"; exit 0; }
NAME="e2econan$RUN_ID"
echo "$NAME" > "$WORK/conan-name"
D=$(dir conan-b)
cat > "$D/conanfile.py" <<PY
from conan import ConanFile
class Pkg(ConanFile):
    name = "$NAME"
    version = "1.0.0"
    settings = "os", "arch", "compiler", "build_type"
    def package(self):
        pass
PY
(cd "$D" && conan profile detect >/dev/null 2>&1)
(cd "$D" && conan create . --user=ci --channel=stable >/dev/null 2>&1) && pass "B: conan create" || { fail "B: conan create"; exit 0; }
(cd "$D" && conan remote add rucoder "$API/pkgs/conan" --force >/dev/null 2>&1; conan remote login rucoder "$WRITE_TOKEN" -p "$WRITE_TOKEN" >/dev/null 2>&1)
(cd "$D" && conan upload "$NAME/1.0.0@ci/stable" -r rucoder -c >/dev/null 2>&1) && pass "B: conan upload (recipe+binary)" || fail "B: conan upload"
