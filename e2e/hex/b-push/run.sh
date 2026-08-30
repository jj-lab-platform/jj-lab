#!/usr/bin/env bash
# B: publish a self-made hex package.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
have mix || { skip "mix not installed"; exit 0; }
SYS_ARCHIVES="/opt/tools/mise/installs/elixir/1.20.3-otp-29/.mix/archives"
[ -d "$SYS_ARCHIVES/hex-2.5.1" ] && export MIX_ARCHIVES="$SYS_ARCHIVES"
D=$(dir hex-arch-hex)
export MIX_HOME="$D/.mix" HEX_HOME="$D/.hex"
mkdir -p "$MIX_HOME" "$HEX_HOME"
mix archive >/dev/null 2>&1
PKGNAME="e2e_hex_${RUN_ID}"
echo "$PKGNAME" > "$WORK/hex-pkgname"
D=$(dir hex-b)
export HEX_MIRROR_URL="$API/pkgs/hex" HEX_UNSAFE_REGISTRY=1 HEX_NO_VERIFY_REPO_ORIGIN=1
mkdir -p "$D/lib"
cat > "$D/mix.exs" <<MX
defmodule HexB.MixProject do
  use Mix.Project
  def project do [app: :$PKGNAME, version: "1.0.0", description: "e2e", package: [licenses: ["MIT"], links: %{}], deps: []] end
end
MX
echo 'defmodule HexB, do: (def v, do: "hello")' > "$D/lib/hex_b.ex"
(cd "$D" && mix hex.build >/dev/null 2>&1) && pass "B: mix hex.build" || fail "B: mix hex.build"
TB=$(ls "$D"/*.tar 2>/dev/null | head -1)
code=$(curl_api -s -o /dev/null -w '%{http_code}' -X POST "$API/pkgs/hex/packages/$PKGNAME/releases" --data-binary @"$TB" -H 'content-type: application/octet-stream')
assert_eq "B: hex publish $PKGNAME" 201 "$code"
