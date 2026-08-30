#!/usr/bin/env bash
# A: pull a public package through pull-through (uv).
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
have uv || { skip "uv not installed"; exit 0; }
D=$(dir py-a)
(cd "$D" && uv venv -q .venv >/dev/null 2>&1)
out=$(cd "$D" && VIRTUAL_ENV="$D/.venv" UV_INDEX_URL="$API/pkgs/pypi/simple/" uv pip install requests --no-cache 2>&1)
[ $? -eq 0 ] && pass "A: uv pip install requests via pull-through" || { fail "A: uv install requests"; echo "$out" | tail -5; }
