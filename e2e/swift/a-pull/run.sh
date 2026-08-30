#!/usr/bin/env bash
# A: SCM-to-registry pull-through (upstream api.spm.swift.org unreachable in
# this env; the git clone path is our documented swift pull-through).
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
source ../../flag.sh
have swift || { skip "swift not installed"; exit 0; }

if [ "$(flag GITHUB_OK)" = "1" ]; then
  out=$(curl_api "$API/pkgs/swift/identifiers?url=https://github.com/apple/swift-argument-parser.git")
  assert_contains "A: identifiers from git url" "$out" 'apple.swift-argument-parser'
  rels=$(curl_api "$API/pkgs/swift/apple/swift-argument-parser" 2>/dev/null)
  VER=$(echo "$rels" | python3 -c "import json,sys; d=json.load(sys.stdin); print(sorted(d['releases'].keys())[-1])" 2>/dev/null || true)
  if [ -n "$VER" ]; then
    code=$(curl_api -o /dev/null -w '%{http_code}' "$API/pkgs/swift/apple/swift-argument-parser/$VER.zip")
    assert_code "A: source archive from git tag ($VER)" 200 "$code"
  else
    skip "A: no releases enumerated from git"
  fi
else
  skip "A: github unreachable for SCM pull-through"
fi
