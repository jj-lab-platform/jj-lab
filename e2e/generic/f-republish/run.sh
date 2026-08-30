#!/usr/bin/env bash
# F: republish — same name/version/file with different bytes must overwrite
# (generic is an append-style store; this must be a replace now).
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
NAME=$(cat "$WORK/generic-name")
D=$(dir generic-f)
echo "v1-bytes" > "$D/a.txt"
echo "v2-bytes" > "$D/b.txt"

curl_api -s -o /dev/null -X PUT "$API/pkgs/generic/$NAME/1.0.0/artifact.txt" --data-binary @"$D/a.txt"
code=$(curl_api -s -o /dev/null -w '%{http_code}' -X PUT "$API/pkgs/generic/$NAME/1.0.0/artifact.txt" --data-binary @"$D/b.txt")
assert_eq "F: generic republish" 201 "$code"

out=$(curl_api "$API/pkgs/generic/$NAME/1.0.0/artifact.txt")
[ "$out" = "v2-bytes" ] && pass "F: overwritten (v2-bytes)" || fail "F: overwritten (got: $out)"

# Delete then republish should restore.
code=$(curl_api -s -o /dev/null -w '%{http_code}' -X DELETE "$API/pkgs/generic/$NAME.version")
assert_eq "F: generic delete" 200 "$code"
code=$(curl_api -s -o /dev/null -w '%{http_code}' -X PUT "$API/pkgs/generic/$NAME/1.0.0/artifact.txt" --data-binary @"$D/b.txt")
assert_eq "F: generic republish after delete" 201 "$code"
out=$(curl_api "$API/pkgs/generic/$NAME/1.0.0/artifact.txt")
[ "$out" = "v2-bytes" ] && pass "F: back after delete+republish" || fail "F: back after delete+republish"