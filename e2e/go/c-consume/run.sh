#!/usr/bin/env bash
# C: consume the module pushed in B.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
have go || { skip "go not installed"; exit 0; }
MOD=$(cat "$WORK/go-mod-name")
D=$(dir go-c)
(cd "$D" && GOPROXY="$API/pkgs/go" GONOSUMDB='*' GOSUMDB=off go mod init example.com/c >/dev/null 2>&1)
mkdir -p "$D"
cat > "$D/main.go" <<M
package main
import ("fmt"; lib "$MOD")
func main(){ fmt.Println(lib.Hello()) }
M
(cd "$D" && GOPROXY="$API/pkgs/go" GONOSUMDB='*' GOSUMDB=off GOFLAGS=-mod=mod go run . >/tmp/goc.out 2>&1; grep -q hello /tmp/goc.out) \
  && pass "C: go run uses $MOD (just pushed)" || fail "C: go run $MOD"
