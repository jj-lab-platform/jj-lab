#!/usr/bin/env bash
# C: consume the package pushed in B.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
have uv || { skip "uv not installed"; exit 0; }
NAME=$(cat "$WORK/py-pkg-name")
D=$(dir py-c)
(cd "$D" && uv venv -q .venv >/dev/null 2>&1)
out=$(cd "$D" && VIRTUAL_ENV="$D/.venv" UV_INDEX_URL="$API/pkgs/pypi/simple/" uv pip install "$NAME==1.0.0" --no-cache 2>&1)
[ $? -eq 0 ] && pass "C: uv install $NAME (just pushed)" || { fail "C: uv install $NAME"; echo "$out" | tail -5; }
