#!/usr/bin/env bash
# D: mutate — retire the version, then hard-delete it (hex.pm phase-2).
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
PKG=$(cat "$WORK/hex-pkgname" | tr - _)
code=$(curl_api -s -o /dev/null -w '%{http_code}' -X POST "$API/pkgs/hex/packages/$PKG/releases/1.0.0/retire")
assert_status_in "D: hex retire" "$code" "200 201"
code=$(curl_api -s -o /dev/null -w '%{http_code}' -X DELETE "$API/pkgs/hex/packages/$PKG/releases/1.0.0")
assert_status_in "D: hex delete release" "$code" "201 404"
