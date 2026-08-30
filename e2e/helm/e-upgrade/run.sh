#!/usr/bin/env bash
# E: upgrade — publish 0.2.0, assert index.yaml lists it.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
have helm || { skip "helm not installed"; exit 0; }
NAME=$(cat "$WORK/helm-name")
D=$(dir helm-e)
mkdir -p "$D/$NAME/templates"
printf 'apiVersion: v2\nname: %s\nversion: 0.2.0\n' "$NAME" > "$D/$NAME/Chart.yaml"
echo 'apiVersion: v1
kind: ConfigMap' > "$D/$NAME/templates/cm.yaml"
(cd "$D" && helm package "$NAME" >/dev/null 2>&1) && pass "E: helm package 0.2.0" || fail "E: helm package 0.2.0"
code=$(curl_api -s -o /dev/null -w '%{http_code}' -X POST "$API/pkgs/helm/api/charts" --data-binary "@$D/$NAME-0.2.0.tgz")
assert_eq "E: helm upload 0.2.0" 201 "$code"
out=$(curl_api "$API/pkgs/helm/index.yaml")
[[ "$out" == *"$NAME-0.2.0.tgz"* ]] && pass "E: index lists 0.2.0" || fail "E: index lists 0.2.0"
