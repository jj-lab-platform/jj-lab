#!/usr/bin/env bash
# C: consume the hex package pushed in B.
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
PKG=$(cat "$WORK/hex-pkgname" | tr - _)
D=$(dir hex-c)
export HEX_MIRROR_URL="$API/pkgs/hex" HEX_UNSAFE_REGISTRY=1 HEX_NO_VERIFY_REPO_ORIGIN=1
mkdir -p "$D"
cat > "$D/mix.exs" <<MX
defmodule HexC.MixProject do
  use Mix.Project
  def project do [app: :hex_c, deps: deps()] end
  defp deps do [{:$PKG, "1.0.0"}] end
end
MX
(cd "$D" && mix deps.get >/dev/null 2>&1) && pass "C: mix deps.get $PKG (just pushed)" || fail "C: mix deps.get $PKG"
