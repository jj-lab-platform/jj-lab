#!/usr/bin/env bash
# B: package + push a chart.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
have helm || { skip "helm not installed"; exit 0; }
NAME="e2ehelm$RUN_ID"
echo "$NAME" > "$WORK/helm-name"
D=$(dir helm-b)
mkdir -p "$D/$NAME/templates"
printf 'apiVersion: v2\nname: %s\nversion: 0.1.0\n' "$NAME" > "$D/$NAME/Chart.yaml"
echo 'apiVersion: v1
kind: ConfigMap' > "$D/$NAME/templates/cm.yaml"
(cd "$D" && helm package "$NAME" >/dev/null 2>&1) && pass "B: helm package" || fail "B: helm package"
code=$(curl_api -s -o /dev/null -w '%{http_code}' -X POST "$API/pkgs/helm/api/charts" --data-binary "@$D/$NAME-0.1.0.tgz")
assert_eq "B: helm upload" 201 "$code"
