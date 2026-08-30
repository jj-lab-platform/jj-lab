#!/usr/bin/env bash
# Upstream connectivity preflight: writes a flags file consumed by other suites.
set -uo pipefail
cd "$(dirname "$0")"
source ../lib.sh
section "net-preflight"

FLAGS="$WORK/upstream.flags"
: > "$FLAGS"

probe() { # probe <flag> <url> [proxy]
  local flag="$1" url="$2" p="${3:-}" code
  if [ -n "$p" ]; then
    code=$(env -u no_proxy -u NO_PROXY curl -s --max-time 12 -o /dev/null -w '%{http_code}' -x "$p" "$url" 2>/dev/null || true)
  else
    code=$(curl -s --max-time 10 -o /dev/null -w '%{http_code}' "$url" 2>/dev/null || true)
  fi
  if [[ "$code" =~ ^[0-9]{3}$ ]] && [ "$code" -ge 200 ] && [ "$code" -lt 500 ]; then
    echo "$flag=1" >> "$FLAGS"
    pass "$flag reachable ($code)"
  else
    echo "$flag=0" >> "$FLAGS"
    skip "$flag unreachable ($code)"
  fi
}

probe NPM_OK    "https://registry.npmjs.org/lodash"
probe PYPI_OK   "https://pypi.org/simple/requests/"
probe CARGO_OK  "https://index.crates.io/config.json"
probe CARGO_STATIC_OK "https://static.crates.io/crates/anyhow/anyhow-1.0.86.crate"
probe MAVEN_OK  "https://repo.maven.apache.org/maven2/junit/junit/maven-metadata.xml"
probe COMPOSER_OK "https://repo.packagist.org/p2/symfony/console.json"
probe NUGET_OK  "https://api.nuget.org/v3/index.json"
probe RUBYGEMS_OK "https://index.rubygems.org/versions"
probe HEX_OK    "https://hex.pm/api/packages/jason"
probe PUB_OK    "https://pub.dev/api/packages/http"
probe CONAN_OK  "https://center2.conan.io/v2/conans/search?q=zlib"
MIHOMO="${MIHOMO_PROXY:-http://mihomo.develop.svc.cluster.local:7890}"
probe DOCKER_OK "https://registry-1.docker.io/v2/" "$MIHOMO"
probe GITHUB_OK "https://github.com/apple/swift-argument-parser" "$MIHOMO"
probe GO_OK "https://proxy.golang.org/github.com/pkg/errors/@v/list" "$MIHOMO"

echo "$PASS $FAIL $SKIP" > "$WORK/net-preflight.result"
summary "net-preflight"
