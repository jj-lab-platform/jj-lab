#!/usr/bin/env bash
# C: consume the gem pushed in B.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
have gem || { skip "gem not installed"; exit 0; }
NAME=$(cat "$WORK/gem-name")
D=$(dir gem-c)
(cd "$D" && HOME="$D" GEM_HOME="$D/.gems" gem install -q --no-document "$NAME" --source "$API/pkgs/rubygems" -v 1.0.0 >/tmp/t.out 2>&1)
[ -d "$D/.gems/gems/$NAME-1.0.0" ] && pass "C: gem install $NAME (just pushed)" || fail "C: gem install $NAME"
