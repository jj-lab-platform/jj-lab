#!/usr/bin/env bash
# A: pull a public package through the registry's pull-through proxy.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
have npm || { skip "npm not installed"; exit 0; }
D=$(dir npm-a)
cat > "$D/package.json" <<PJ
{"name":"npm-a","private":true,"dependencies":{"is-even":"1.0.0"}}
PJ
cat > "$D/.npmrc" <<NRC
registry=$API/pkgs/npm
loglevel=error
fetch-retries=1
NRC
out=$(cd "$D" && npm install --no-audit --no-fund --prefer-online 2>&1)
[ $? -eq 0 ] && pass "A: npm install is-even via pull-through" || { fail "A: npm install is-even"; echo "$out" | tail -5; }
[ -f "$D/node_modules/is-even/package.json" ] && pass "A: package on disk" || fail "A: package missing on disk"
