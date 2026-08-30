#!/usr/bin/env bash
# C: download the artifact pushed in B.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
NAME=$(cat "$WORK/generic-name")
out=$(curl_api -s "$API/pkgs/generic/$NAME/1.0.0/artifact.txt")
[ "$out" = "hello-generic" ] && pass "C: generic download $NAME (just pushed)" || fail "C: generic download"
