#!/usr/bin/env bash
# C: pull the chart pushed in B.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
have helm || { skip "helm not installed"; exit 0; }
NAME=$(cat "$WORK/helm-name")
D=$(dir helm-c)
export HELM_CONFIG_HOME="$D/cfg" HELM_CACHE_HOME="$D/cache" HELM_DATA_HOME="$D/data"
mkdir -p "$HELM_CONFIG_HOME" "$HELM_CACHE_HOME" "$HELM_DATA_HOME"
(cd "$D" && helm repo add rucoder "$API/pkgs/helm" >/dev/null 2>&1 && helm repo update rucoder >/dev/null 2>&1 && helm pull "rucoder/$NAME" --version 0.1.0 >/dev/null 2>&1) \
  && pass "C: helm pull $NAME (just pushed)" || fail "C: helm pull $NAME"
