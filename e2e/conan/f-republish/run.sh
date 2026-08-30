#!/usr/bin/env bash
# F: republish — same recipe file with different bytes must overwrite on the
# server (conan's store_file was append-style and is now replace-style).
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
NAME=$(cat "$WORK/conan-name")
D=$(dir conan-f)

printf 'from conan import ConanFile\n# v1\n' > "$D/a.py"
printf 'from conan import ConanFile\n# v2\n' > "$D/b.py"

curl_api -s -o /dev/null -X PUT "$API/pkgs/conan/v2/conans/$NAME/1.0.0/_/_/revisions/r1/files/conanfile.py" --data-binary @"$D/a.py"
code=$(curl_api -s -o /dev/null -w '%{http_code}' -X PUT "$API/pkgs/conan/v2/conans/$NAME/1.0.0/_/_/revisions/r1/files/conanfile.py" --data-binary @"$D/b.py")
assert_status_in "F: conan republish conanfile.py" "$code" "200 201"

out=$(curl_api "$API/pkgs/conan/v2/conans/$NAME/1.0.0/_/_/revisions/r1/files/conanfile.py")
echo "$out" | grep -q '# v2' && pass "F: conanfile.py overwritten" || fail "F: conanfile.py overwritten"
echo "$out" | grep -q '# v1' && fail "F: stale bytes leaked" || pass "F: no stale bytes"