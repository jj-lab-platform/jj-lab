#!/usr/bin/env bash
# E: upgrade — publish 1.0.1, assert /simple/ index lists both.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
have uv || { skip "uv not installed"; exit 0; }
NAME=$(cat "$WORK/py-pkg-name")
D=$(dir py-e)
cat > "$D/pyproject.toml" <<PY
[project]
name = "$NAME"
version = "1.0.1"
PY
(cd "$D" && uv build -q >/dev/null 2>&1) || fail "E: uv build 1.0.1"
WHL=$(ls "$D/dist/"*.whl 2>/dev/null | head -1)
code=$(curl_api -s -o /dev/null -w '%{http_code}' -X POST "$API/pkgs/pypi/upload" -F "name=$NAME" -F "version=1.0.1" -F "content=@$WHL")
assert_eq "E: pypi upload 1.0.1" 201 "$code"
out=$(curl_api "$API/pkgs/pypi/simple/$NAME/" -H 'Accept: application/vnd.pypi.simple.v1+json')
echo "$out" | python3 -c "import json,sys; print('\n'.join(json.load(sys.stdin)['versions']))" | grep -q '1.0.1' \
  && pass "E: /simple/ lists 1.0.1" || fail "E: /simple/ lists 1.0.1"
