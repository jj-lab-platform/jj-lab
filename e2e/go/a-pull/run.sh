#!/usr/bin/env bash
# A: pull a public module through pull-through.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
have go || { skip "go not installed"; exit 0; }
D=$(dir go-a)
(cd "$D" && GOPROXY="$API/pkgs/go" GONOSUMDB='*' GOSUMDB=off go mod init example.com/a >/dev/null 2>&1)
(cd "$D" && GOPROXY="$API/pkgs/go" GONOSUMDB='*' GOSUMDB=off go get github.com/google/uuid@v1.6.0 >/dev/null 2>&1) \
  && pass "A: go get google/uuid via pull-through" || fail "A: go get google/uuid"
