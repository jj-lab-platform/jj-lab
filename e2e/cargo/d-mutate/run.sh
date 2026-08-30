#!/usr/bin/env bash
# D: mutate — cargo yank 1.0.0, assert index yanked:true, then unyank.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
have cargo || { skip "cargo not installed"; exit 0; }
NAME=$(cat "$WORK/cargo-pkg-name")
D=$(dir cargo-d)
mkdir -p "$D/.cargo"
cat > "$D/.cargo/config.toml" <<C
[registries.rucoder]
index = "sparse+$API/pkgs/cargo/"
token = "${TOKEN:-test}"
C
(cd "$D" && CARGO_HOME="$D/.ch" cargo yank --version 1.0.0 --registry rucoder "$NAME" >/dev/null 2>&1) \
  && pass "D: cargo yank $NAME" || fail "D: cargo yank $NAME"
out=$(curl_api "$API/pkgs/cargo/$NAME")
echo "$out" | grep -q '"yanked":true' && pass "D: sparse index yanked:true" || fail "D: sparse index yanked:true"
(cd "$D" && CARGO_HOME="$D/.ch" cargo yank --undo --version 1.0.0 --registry rucoder "$NAME" >/dev/null 2>&1) \
  && pass "D: cargo unyank $NAME" || fail "D: cargo unyank $NAME"
