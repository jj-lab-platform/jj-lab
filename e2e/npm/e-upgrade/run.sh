#!/usr/bin/env bash
# E: upgrade — publish 1.0.1, assert dist-tags.latest moves.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
have npm || { skip "npm not installed"; exit 0; }
NAME=$(cat "$WORK/npm-pkg-name")
D=$(dir npm-e)
cat > "$D/package.json" <<PJ
{"name":"$NAME","version":"1.0.1","description":"e2e","main":"index.js"}
PJ
echo "module.exports=()=>'hello-new-$NAME';" > "$D/index.js"
AUTH_URL=$(python3 -c "import urllib.parse,sys; u=urllib.parse.urlparse(sys.argv[1]); print('//'+u.netloc+u.path.rstrip('/')+'/')" "$API/pkgs/npm/")
cat > "$D/.npmrc" <<NRC
registry=$API/pkgs/npm/
${AUTH_URL}:_authToken=${TOKEN:-test}
always-auth=true
loglevel=error
NRC
(cd "$D" && npm publish --access public 2>/dev/null) && pass "E: npm publish 1.0.1" || fail "E: npm publish 1.0.1"
out=$(curl_api "$API/pkgs/npm/$NAME")
echo "$out" | python3 -c "import json,sys; d=json.load(sys.stdin); print(d.get('dist-tags',{}).get('latest'))" | grep -q '^1.0.1$' \
  && pass "E: dist-tags.latest == 1.0.1" || fail "E: dist-tags.latest == 1.0.1"

# Cross-version semantic ordering: lexically "1.0.10" < "1.0.9" and
# "10.0.0" < "2.0.0", but semantically the reverse. The latest tag must
# follow semantic order, not the store's lexical order.
for VER in 1.0.9 1.0.10 2.0.0 10.0.0; do
  mkdir -p "$D/v$VER"
  printf '{"name":"%s","version":"%s","description":"e2e","main":"index.js"}' "$NAME" "$VER" > "$D/v$VER/package.json"
  echo "module.exports=()=>'$VER';" > "$D/v$VER/index.js"
  (cd "$D/v$VER" && cp "$D/.npmrc" .npmrc && npm publish --access public 2>/dev/null) \
    && pass "E: npm publish $VER" || fail "E: npm publish $VER"
done
out=$(curl_api "$API/pkgs/npm/$NAME")
echo "$out" | python3 -c "import json,sys; d=json.load(sys.stdin); print(d.get('dist-tags',{}).get('latest'))" | grep -q '^10.0.0$' \
  && pass "E: latest == 10.0.0 (semantic order)" || fail "E: latest == 10.0.0 (semantic order)"
