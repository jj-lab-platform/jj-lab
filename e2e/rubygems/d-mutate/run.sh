#!/usr/bin/env bash
# D: mutate — gem yank the pushed version, assert install fails, then re-push.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
have gem || { skip "gem not installed"; exit 0; }
NAME=$(cat "$WORK/gem-name")
D=$(dir gem-d)
mkdir -p "$D/.gem"
echo ":dummy: ${TOKEN:-test}" > "$D/.gem/credentials"
chmod 600 "$D/.gem/credentials"
HOME="$D" gem yank "$NAME" -v 1.0.0 --host "$API/pkgs/rubygems" --key dummy >/dev/null 2>&1 \
  && pass "D: gem yank $NAME" || fail "D: gem yank $NAME"
code=$(curl_api -s -o /dev/null -w '%{http_code}' "$API/pkgs/rubygems/gems/$NAME-1.0.0.gem")
assert_status_in "D: gem file gone after yank" "$code" "404"
