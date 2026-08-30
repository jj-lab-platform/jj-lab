#!/usr/bin/env bash
# C: consume the package pushed in B.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
have npm || { skip "npm not installed"; exit 0; }
NAME=$(cat "$WORK/npm-pkg-name")
D=$(dir npm-c)
cat > "$D/package.json" <<PJ
{"name":"npm-c","private":true,"dependencies":{"$NAME":"1.0.0"}}
PJ
cat > "$D/.npmrc" <<NRC
registry=$API/pkgs/npm/
loglevel=error
NRC
out=$(cd "$D" && npm install --no-audit --no-fund 2>&1)
[ $? -eq 0 ] && pass "C: npm install $NAME (just pushed)" || { fail "C: npm install $NAME"; echo "$out" | tail -8; }
(cd "$D" && node -e "const p=require('$NAME');if(p()!=='hello-$NAME')process.exit(1)") 2>/dev/null \
  && pass "C: module executes" || fail "C: module execute"
