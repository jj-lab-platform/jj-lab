#!/usr/bin/env bash
# A: pull a public hex package through pull-through.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
have mix || { skip "mix not installed"; exit 0; }
# Use the preinstalled OTP-wide hex archive (builds.hex.pm is unreachable here
# and a fresh MIX_ARCHIVES forces a re-download).
SYS_ARCHIVES="/opt/tools/mise/installs/elixir/1.20.3-otp-29/.mix/archives"
[ -d "$SYS_ARCHIVES/hex-2.5.1" ] && export MIX_ARCHIVES="$SYS_ARCHIVES"
D=$(dir hex-arch-hex)
export MIX_HOME="$D/.mix" HEX_HOME="$D/.hex"
mkdir -p "$MIX_HOME" "$HEX_HOME"
mix archive >/dev/null 2>&1
D=$(dir hex-a)
export HEX_MIRROR_URL="$API/pkgs/hex" HEX_UNSAFE_REGISTRY=1 HEX_NO_VERIFY_REPO_ORIGIN=1
mkdir -p "$D"
cat > "$D/mix.exs" <<'MX'
defmodule HexA.MixProject do
  use Mix.Project
  def project do [app: :hex_a, deps: deps()] end
  defp deps do [{:jason, "~> 1.4"}] end
end
MX
(cd "$D" && mix deps.get >/dev/null 2>&1) && pass "A: mix deps.get jason via pull-through" || fail "A: mix deps.get jason"
