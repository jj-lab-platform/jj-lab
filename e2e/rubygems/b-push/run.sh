#!/usr/bin/env bash
# B: publish a self-made gem.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
have gem || { skip "gem not installed"; exit 0; }
NAME="e2e-gem-$RUN_ID"
echo "$NAME" > "$WORK/gem-name"
D=$(dir gem-b)
cat > "$D/$NAME.gemspec" <<GS
Gem::Specification.new do |s|
  s.name = '$NAME'
  s.version = '1.0.0'
  s.summary = 'e2e'
  s.authors = ['t']
  s.files = ['lib/e.rb']
end
GS
mkdir -p "$D/lib"
echo "module E2eGem; end" > "$D/lib/e.rb"
(cd "$D" && gem build "$NAME.gemspec" -q >/dev/null 2>&1) && pass "B: gem build" || fail "B: gem build"
mkdir -p "$D/.gem"
echo ":dummy: ${TOKEN:-test}" > "$D/.gem/credentials"
chmod 600 "$D/.gem/credentials"
HOME="$D" gem push "$D/$NAME-1.0.0.gem" --host "$API/pkgs/rubygems" --key dummy >/dev/null 2>&1 && pass "B: gem push" || fail "B: gem push"
