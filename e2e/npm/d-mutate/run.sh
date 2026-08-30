#!/usr/bin/env bash
# D: mutate — npm deprecate the published version, then undo.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
have npm || { skip "npm not installed"; exit 0; }
NAME=$(cat "$WORK/npm-pkg-name")
D=$(dir npm-d)
AUTH_URL=$(python3 -c "import urllib.parse,sys; u=urllib.parse.urlparse(sys.argv[1]); print('//'+u.netloc+u.path.rstrip('/')+'/')" "$API/pkgs/npm/")
cat > "$D/.npmrc" <<NRC
registry=$API/pkgs/npm/
${AUTH_URL}:_authToken=${TOKEN:-test}
always-auth=true
loglevel=error
NRC
mkdir -p "$D"
(cd "$D" && npm deprecate "$NAME@1.0.0" "legacy version" 2>/dev/null) \
  && pass "D: npm deprecate $NAME" || fail "D: npm deprecate $NAME"
out=$(curl_api "$API/pkgs/npm/$NAME")
case "$out" in *'"deprecated":"legacy version"'*) pass "D: packument deprecated flag";; *) fail "D: packument deprecated flag";; esac
(cd "$D" && npm deprecate "$NAME@1.0.0" "" 2>/dev/null) \
  && pass "D: npm un-deprecate" || fail "D: npm un-deprecate"
