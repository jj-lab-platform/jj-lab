#!/usr/bin/env bash
# F: republish — same version with different wheel must overwrite and be
# served back (not the first-published bytes).
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
NAME=$(cat "$WORK/py-pkg-name")
D=$(dir py-f)
cat > "$D/pyproject.toml" <<PY
[project]
name = "$NAME"
version = "1.0.0"
PY

# Build a first wheel (v1), then a second distinct wheel (v2), both "1.0.0".
mkdir -p "$D/v1/$NAME"
printf 'HELLO_V1 = "one"\n' > "$D/v1/lib.py"
cat > "$D/v1/pyproject.toml" <<PY
[project]
name = "$NAME"
version = "1.0.0"
PY
mkdir -p "$D/v2/$NAME"
printf 'HELLO_V1 = "two"\n' > "$D/v2/lib.py"
cat > "$D/v2/pyproject.toml" <<PY
[project]
name = "$NAME"
version = "1.0.0"
PY

(cd "$D/v1" && uv build -q >/dev/null 2>&1)
(cd "$D/v2" && uv build -q >/dev/null 2>&1)
W1=$(ls "$D/v1/dist/"*.whl 2>/dev/null | head -1)
W2=$(ls "$D/v2/dist/"*.whl 2>/dev/null | head -1)
[ -f "$W2" ] || W2=$(ls "$D/v2/dist/"*.tar.gz 2>/dev/null | head -1)

# First publish, then republish with the second wheel. The simple index (and
# stored blob) must reflect the second bytes.
curl_api -s -o /dev/null -X POST "$API/pkgs/pypi/upload" -F "name=$NAME" -F "version=1.0.0" -F "content=@$W1"
code=$(curl_api -s -o /dev/null -w '%{http_code}' -X POST "$API/pkgs/pypi/upload" -F "name=$NAME" -F "version=1.0.0" -F "content=@$W2")
assert_eq "F: pypi republish 1.0.0" 201 "$code"

FNAME=$(basename "$W2")
curl_api "$API/pkgs/pypi/simple/$NAME/$FNAME" -o "$D/dl.whl"
# wheel is a zip; METADATA contains "Version: 1.0.0". Verify content mismatch
# via byte compare against v2 (the overwrite happened if it matches v2).
cmp -s "$D/dl.whl" "$W2" && pass "F: wheel overwritten" || fail "F: wheel overwritten"

# Name normalization: "Foo.Bar" / "Foo-Bar" / "Foo_Bar" collapse to "foo-bar"
# under PEP 503, so re-publishing under a differently-spelled name must hit
# the same project (no duplicate repository).
NORM=$(echo "$NAME" | tr '_' '-')
ALT=$(echo "$NAME" | tr 'a-z' 'A-Z' | tr '_' '.')
code=$(curl_api -s -o /dev/null -w '%{http_code}' -X POST "$API/pkgs/pypi/upload" -F "name=$ALT" -F "version=1.0.0" -F "content=@$W2")
assert_status_in "F: pypi normalized-name republish" "$code" "200 201"
code=$(curl_api -s -o /dev/null -w '%{http_code}' "$API/pkgs/pypi/simple/$NORM/")
assert_status_in "F: normalized name serves" "$code" "200"