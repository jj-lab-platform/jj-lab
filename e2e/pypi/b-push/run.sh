#!/usr/bin/env bash
# B: publish a self-made wheel via twine/uv-build + HTTP upload.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
have uv || { skip "uv not installed"; exit 0; }
NAME="e2e-py-$RUN_ID"
echo "$NAME" > "$WORK/py-pkg-name"
D=$(dir py-b)
cat > "$D/pyproject.toml" <<PY
[project]
name = "$NAME"
version = "1.0.0"
PY
(cd "$D" && uv build -q >/dev/null 2>&1) && pass "B: uv build" || fail "B: uv build"
WHL=$(ls "$D/dist/"*.whl 2>/dev/null | head -1) || WHL=$(ls "$D/dist/"*.tar.gz 2>/dev/null | head -1)
code=$(curl_api -s -o /dev/null -w '%{http_code}' -X POST "$API/pkgs/pypi/upload" -F "name=$NAME" -F "version=1.0.0" -F "content=@$WHL")
assert_eq "B: pypi upload $NAME" 201 "$code"
