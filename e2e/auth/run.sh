#!/usr/bin/env bash
# jjlab token-auth matrix: anonymous (read-only) vs write token across
# protocols. The deployed service uses JJLAB_TOKENS="devtoken=write".
set -uo pipefail
cd "$(dirname "$0")"
source ../lib.sh
source ../flag.sh

section "auth"

AU="$API/pkgs/system"
AO="$API/v2"
AN="$API/pkgs/npm"
ANU="$API/pkgs/nuget"
WRITE="$WRITE_TOKEN"

# 1) anonymous read OK
code=$(curl_anon -o /dev/null -w '%{http_code}' "$AU/upstreams")
assert_code "anon read: system upstreams 200" 200 "$code"

# 2) anonymous write denied
code=$(curl_anon -o /dev/null -w '%{http_code}' -X PUT "$AU/upstreams/npm" -H 'Content-Type: application/json' -d '{"url":"https://x.example"}')
assert_code "anon write: system PUT 401" 401 "$code"

# 3) write token (Authorization: token <t>) write OK
code=$(curl_api -o /dev/null -w '%{http_code}' -X PUT "$AU/upstreams/npm" -H 'Content-Type: application/json' -d '{"url":"https://registry.npmjs.org"}')
assert_status_in "write token: system PUT" "$code" "200 201"

# 4) npm: anonymous publish denied, bearer token publish OK
code=$(curl_anon -o /dev/null -w '%{http_code}' -X PUT "$AN/x-auth-deny" -H 'Content-Type: application/json' -d '{"name":"x-auth-deny","versions":{"1.0.0":{"name":"x-auth-deny","version":"1.0.0"}}}')
assert_code "anon npm publish 401" 401 "$code"
code=$(curl_api -o /dev/null -w '%{http_code}' -X PUT "$AN/x-auth-ok" -H 'Content-Type: application/json' -H "Authorization: Bearer $WRITE" -d '{"name":"x-auth-ok","versions":{"1.0.0":{"name":"x-auth-ok","version":"1.0.0"}}}')
assert_status_in "bearer token npm publish" "$code" "200 201"

# 5) OCI: anonymous /v2/ 401 challenge; token flow allows push.
#    The realm must carry JJLAB_SELF_BASE (the cluster name, not 127.0.0.1).
code=$(curl_anon -o /dev/null -w '%{http_code}' "$AO/")
assert_code "OCI anon ping 401" 401 "$code"
CH=$(curl_anon -I "$AO/" 2>/dev/null | grep -i www-authenticate | head -1)
assert_contains "OCI WWW-Authenticate challenge" "$CH" 'Bearer realm'
assert_contains "OCI realm uses self_base" "$CH" 'jj-lab.temp.svc.cluster.local'
TOK=$(curl_api "$API/v2/token?service=oci-registry&scope=repository:x-auth-oci:push,pull" -H "Authorization: token $WRITE" | python3 -c "import json,sys; print(json.load(sys.stdin)['token'])" 2>/dev/null)
[ -n "$TOK" ] && pass "OCI token issued" || fail "OCI token issued"
echo -n "auth-blob-data" > "$WORK/auth-blob"
BD="sha256:$(sha256sum "$WORK/auth-blob" | cut -d' ' -f1)"
code=$(curl_api -o /dev/null -w '%{http_code}' -X POST --data-binary @"$WORK/auth-blob" "$AO/x-auth-oci/blobs/uploads/?digest=$BD" -H "Authorization: Bearer $TOK")
assert_status_in "OCI push with token" "$code" "200 201"

# 6) nuget: anonymous push denied, X-NuGet-ApiKey (write token) push OK
python3 - <<PYEOF
import zipfile
z = zipfile.ZipFile('$WORK/xcli-auth.nupkg','w')
z.writestr('xcli.auth.nuspec', '<package><metadata><id>Xcli.Auth</id><version>1.0.0</version></metadata></package>')
z.close()
PYEOF
code=$(curl_anon -o /dev/null -w '%{http_code}' -X PUT --data-binary @"$WORK/xcli-auth.nupkg" "$ANU/api/v2/package")
assert_code "anon nuget push 401" 401 "$code"
code=$(curl_api -o /dev/null -w '%{http_code}' -X PUT --data-binary @"$WORK/xcli-auth.nupkg" -H "X-NuGet-ApiKey: $WRITE" "$ANU/api/v2/package")
assert_status_in "X-NuGet-ApiKey push" "$code" "200 201"

# 7) skopeo push with creds OK; anonymous denied. Source is our own registry's
#    pull-through cache of alpine (client never talks to docker.io directly).
if have skopeo && [ "$(flag DOCKER_OK)" = "1" ]; then
  if skopeo copy --src-tls-verify=false --dest-tls-verify=false --dest-creds "x:$WRITE" "docker://jj-lab.temp.svc.cluster.local/library/alpine:latest" "docker://jj-lab.temp.svc.cluster.local/x-auth/alpine:ok" >/dev/null 2>&1; then
    pass "skopeo push with creds"
  else
    fail "skopeo push with creds"
  fi
  if skopeo copy --src-tls-verify=false --dest-tls-verify=false "docker://jj-lab.temp.svc.cluster.local/library/alpine:latest" "docker://jj-lab.temp.svc.cluster.local/x-auth/alpine:denied" >/dev/null 2>&1; then
    fail "skopeo push anon should fail"
  else
    pass "skopeo push anon denied"
  fi
else
  skip "skopeo/docker.io unavailable"
fi

echo "$PASS $FAIL $SKIP" > "$WORK/auth.result"
summary "auth"
