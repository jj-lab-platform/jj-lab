#!/usr/bin/env bash
# B: publish a self-made package (C#).
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
have dotnet || { skip "dotnet not installed"; exit 0; }
ID="E2eNuget$RUN_ID"
echo "$ID" > "$WORK/nuget-name"
D=$(dir nuget-b)
cat > "$D/lib.csproj" <<CSP
<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>net10.0</TargetFramework>
    <PackageId>$ID</PackageId>
    <Version>1.0.0</Version>
  </PropertyGroup>
</Project>
CSP
cat > "$D/nuget.config" <<NX
<?xml version="1.0" encoding="utf-8"?>
<configuration>
  <packageSources>
    <add key="rucoder" value="$API/pkgs/nuget/v3/index.json" allowInsecureConnections="true" />
  </packageSources>
</configuration>
NX
(cd "$D" && DOTNET_CLI_HOME="$D/.dotnet" dotnet pack -o out >/dev/null 2>&1) && pass "B: dotnet pack" || fail "B: dotnet pack"
PKG="$D/out/$ID.1.0.0.nupkg"
if [ -f "$PKG" ]; then
  (cd "$D" && DOTNET_CLI_HOME="$D/.dotnet" dotnet nuget push "$PKG" --source rucoder --configfile nuget.config --api-key "${TOKEN:-test}" >/dev/null 2>&1) && pass "B: nuget push" || fail "B: nuget push"
else
  fail "B: nupkg not produced"
fi
