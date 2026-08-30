#!/usr/bin/env bash
# F: republish — same version, different content must overwrite (not append).
# `npm publish` refuses to overwrite an existing version, so we exercise the
# raw-tarball publish path (PUT {pkg}) which the CLI sends for packages and
# which the server treats as authoritative.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
have npm || { skip "npm not installed"; exit 0; }
NAME=$(cat "$WORK/npm-pkg-name")
D=$(dir npm-f)

# Re-publish 1.0.0 with DIFFERENT tarball content via raw tarball PUT.
mkdir -p "$D/v1"
cat > "$D/v1/package.json" <<PJ
{"name":"$NAME","version":"1.0.0","description":"e2e","main":"index.js"}
PJ
echo "module.exports=()=>'Overwritten-$NAME';" > "$D/v1/index.js"
(cd "$D/v1" && tar -czf "$D/t.tgz" package.json index.js)
code=$(curl_api -s -o /dev/null -w '%{http_code}' -X PUT "$API/pkgs/npm/$NAME" --data-binary @"$D/t.tgz" -H 'content-type: application/octet-stream')
assert_eq "F: npm republish 1.0.0 (raw tarball)" 201 "$code"

# Tarball bytes must reflect the new content.
TB="$NAME-1.0.0.tgz"
curl_api "$API/pkgs/npm/$NAME/-/$TB" -o "$D/dl.tgz"
tar -xzf "$D/dl.tgz" -C "$D" index.js 2>/dev/null
grep -q "Overwritten-$NAME" "$D/index.js" && pass "F: tarball overwritten" || fail "F: tarball overwritten"

# Delete + republish should be a clean slate.
AUTH_URL=$(python3 -c "import urllib.parse,sys; u=urllib.parse.urlparse(sys.argv[1]); print('//'+u.netloc+u.path.rstrip('/')+'/')" "$API/pkgs/npm/")
cat > "$D/.npmrc" <<NRC
registry=$API/pkgs/npm/
${AUTH_URL}:_authToken=${TOKEN:-test}
always-auth=true
loglevel=error
NRC
code=$(curl_api -s -o /dev/null -w '%{http_code}' -X DELETE "$API/pkgs/npm/$NAME/-rev/1")
assert_status_in "F: npm unpublish" "$code" "200 404"
(cd "$D/v1" && cp "$D/.npmrc" .npmrc && npm publish --access public 2>/dev/null) \
  && pass "F: npm republish after delete" || fail "F: npm republish after delete"
out=$(curl_api "$API/pkgs/npm/$NAME")
echo "$out" | python3 -c "import json,sys; print('1.0.0' in json.load(sys.stdin).get('versions',{}))" | grep -q '^True$' \
  && pass "F: version back after delete+republish" || fail "F: version back after delete+republish"