#!/usr/bin/env bash
# A: pull a public gem.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
have gem || { skip "gem not installed"; exit 0; }
D=$(dir gem-a)
mkdir -p "$D"
(cd "$D" && HOME="$D" GEM_HOME="$D/.gems" gem install -q --no-document --source "$API/pkgs/rubygems" rake >/tmp/t.out 2>&1)
[ -d "$D/.gems/gems/rake-"* ] 2>/dev/null && pass "A: gem install rake via pull-through" || fail "A: gem install rake"
