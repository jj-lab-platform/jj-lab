#!/usr/bin/env bash
# E: upgrade — publish 1.0.1, assert releases map includes it.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
have swift || { skip "swift not installed"; exit 0; }
P=$(cat "$WORK/swift-pkg-dir" 2>/dev/null)
[ -n "$P" ] || { skip "B did not run"; exit 0; }
REG=$(cat "$WORK/swift-reg" 2>/dev/null)
D=$(dir swift-e)
CFG="$D/.swiftpm-cfg"; SEC="$D/.swiftpm-sec"; CCH="$D/.swiftpm-cache"
mkdir -p "$CFG" "$SEC" "$CCH"
export HOME="$D/.home"; mkdir -p "$HOME"
(cd "$P" && swift package-registry login "$REG" --token "$WRITE_TOKEN" --no-confirm --config-path "$CFG" --security-path "$SEC" --cache-path "$CCH" >/dev/null 2>&1)
cd "$P" && timeout 180 swift package-registry publish --config-path "$CFG" --security-path "$SEC" --cache-path "$CCH" xcliorg.xclikit 1.0.1 --url "$REG" >/dev/null 2>&1 \
  && pass "E: swift publish 1.0.1" || fail "E: swift publish 1.0.1"
out=$(curl_api "$API/pkgs/swift/xcliorg/xclikit" -H 'Accept: application/vnd.swift.registry.v1+json')
echo "$out" | grep -q '"1.0.1"' && pass "E: releases includes 1.0.1" || fail "E: releases includes 1.0.1"
