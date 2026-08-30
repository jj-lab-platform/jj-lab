#!/usr/bin/env bash
# A: verify helm repo index endpoint is served (pull-through via repo update).
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
have helm || { skip "helm not installed"; exit 0; }
D=$(dir helm-a)
export HELM_CONFIG_HOME="$D/cfg" HELM_CACHE_HOME="$D/cache" HELM_DATA_HOME="$D/data"
mkdir -p "$HELM_CONFIG_HOME" "$HELM_CACHE_HOME" "$HELM_DATA_HOME"
(cd "$D" && helm repo add rucoder "$API/pkgs/helm" >/dev/null 2>&1 && helm repo update rucoder >/dev/null 2>&1) \
  && pass "A: helm repo update (index.yaml)" || fail "A: helm repo update"
