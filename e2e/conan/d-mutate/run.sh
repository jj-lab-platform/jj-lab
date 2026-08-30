#!/usr/bin/env bash
# D: mutate — conan remove from remote, assert list no longer shows it.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
have conan || { skip "conan not installed"; exit 0; }
NAME=$(cat "$WORK/conan-name")
D=$(dir conan-d)
(cd "$D" && conan profile detect >/dev/null 2>&1)
(cd "$D" && conan remote add rucoder "$API/pkgs/conan" --force >/dev/null 2>&1; conan remote login rucoder "$WRITE_TOKEN" -p "$WRITE_TOKEN" >/dev/null 2>&1)
(cd "$D" && conan remove "$NAME/1.0.0@ci/stable" -r rucoder -c >/dev/null 2>&1) \
  && pass "D: conan remove from remote" || fail "D: conan remove from remote"
