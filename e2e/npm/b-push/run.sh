#!/usr/bin/env bash
# B: publish a self-made package.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
have npm || { skip "npm not installed"; exit 0; }
NAME="e2e-npm-$RUN_ID"
echo "$NAME" > "$WORK/npm-pkg-name"
D=$(dir npm-b)
cat > "$D/package.json" <<PJ
{"name":"$NAME","version":"1.0.0","description":"e2e","main":"index.js"}
PJ
echo "module.exports=()=>'hello-$NAME';" > "$D/index.js"
AUTH_URL=$(python3 -c "import urllib.parse,sys; u=urllib.parse.urlparse(sys.argv[1]); print('//'+u.netloc+u.path.rstrip('/')+'/')" "$API/pkgs/npm/")
cat > "$D/.npmrc" <<NRC
registry=$API/pkgs/npm/
${AUTH_URL}:_authToken=${TOKEN:-test}
loglevel=error
always-auth=true
NRC
out=$(cd "$D" && npm publish --access public 2>&1)
[ $? -eq 0 ] && pass "B: npm publish $NAME" || { fail "B: npm publish"; echo "$out" | tail -8; }
