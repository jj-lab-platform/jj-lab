#!/usr/bin/env bash
# E: upgrade — build+push 1.0.1, assert versions index lists it.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
have gem || { skip "gem not installed"; exit 0; }
NAME=$(cat "$WORK/gem-name")
D=$(dir gem-e)
cat > "$D/$NAME.gemspec" <<GS
Gem::Specification.new do |s|
  s.name = '$NAME'
  s.version = '1.0.1'
  s.summary = 'e2e'
  s.authors = ['t']
  s.files = ['lib/e.rb']
end
GS
mkdir -p "$D/lib"
echo "module E2eGem; end" > "$D/lib/e.rb"
(cd "$D" && gem build "$NAME.gemspec" -q >/dev/null 2>&1) || fail "E: gem build 1.0.1"
mkdir -p "$D/.gem"
echo ":dummy: ${TOKEN:-test}" > "$D/.gem/credentials"
chmod 600 "$D/.gem/credentials"
HOME="$D" gem push "$D/$NAME-1.0.1.gem" --host "$API/pkgs/rubygems" --key dummy >/dev/null 2>&1 \
  && pass "E: gem push 1.0.1" || fail "E: gem push 1.0.1"
out=$(curl_api "$API/pkgs/rubygems/api/v1/versions/$NAME.json")
echo "$out" | grep -q '1.0.1' && pass "E: versions includes 1.0.1" || fail "E: versions includes 1.0.1"
