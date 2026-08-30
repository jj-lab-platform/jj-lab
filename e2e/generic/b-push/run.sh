#!/usr/bin/env bash
# B: upload a generic artifact.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
NAME="e2egeneric$RUN_ID"
echo "$NAME" > "$WORK/generic-name"
D=$(dir generic-b)
echo "hello-generic" > "$D/artifact.txt"
code=$(curl_api -s -o /dev/null -w '%{http_code}' -X PUT "$API/pkgs/generic/$NAME/1.0.0/artifact.txt" --data-binary @"$D/artifact.txt")
assert_eq "B: generic upload" 201 "$code"
