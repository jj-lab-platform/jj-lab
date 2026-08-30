#!/usr/bin/env bash
# E: upgrade — publish 1.0.1 tarball, assert /versions lists it.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
have mix || { skip "mix not installed"; exit 0; }
PKG=$(cat "$WORK/hex-pkgname")
# Reuse the preinstalled OTP-wide hex archive (builds.hex.pm unreachable).
SYS_ARCHIVES="/opt/tools/mise/installs/elixir/1.20.3-otp-29/.mix/archives"
[ -d "$SYS_ARCHIVES/hex-2.5.1" ] && export MIX_ARCHIVES="$SYS_ARCHIVES"
D=$(dir hex-arch-hex)
export MIX_HOME="$D/.mix" HEX_HOME="$D/.hex"
mkdir -p "$MIX_HOME" "$HEX_HOME"
mix archive >/dev/null 2>&1
export HEX_MIRROR_URL="$API/pkgs/hex" HEX_UNSAFE_REGISTRY=1 HEX_NO_VERIFY_REPO_ORIGIN=1
B=$(dir hex-e)
mkdir -p "$B/lib"
cat > "$B/mix.exs" <<MX
defmodule HexE.MixProject do
  use Mix.Project
  def project do [app: :$PKG, version: "1.0.1", description: "e2e", package: [licenses: ["MIT"], links: %{}], deps: []] end
end
MX
echo 'defmodule HexE, do: (def v, do: "hello2")' > "$B/lib/hex_e.ex"
(cd "$B" && mix hex.build >/dev/null 2>&1) || fail "E: mix hex.build 1.0.1"
TB=$(ls "$B"/*.tar 2>/dev/null | head -1)
code=$(curl_api -s -o /dev/null -w '%{http_code}' -X POST "$API/pkgs/hex/packages/$PKG/releases" --data-binary @"$TB" -H 'content-type: application/octet-stream')
assert_eq "E: hex publish 1.0.1" 201 "$code"
# /versions returns signed-gzipped protobuf (binary): fetch to a file and
# scan the decompressed bytes for the version string.
curl_api -s "$API/pkgs/hex/versions" -o "$B/versions.gz"
gunzip -c "$B/versions.gz" 2>/dev/null | strings | grep -q '1.0.1' && pass "E: versions includes 1.0.1" || fail "E: versions includes 1.0.1"
