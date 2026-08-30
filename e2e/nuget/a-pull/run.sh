#!/usr/bin/env bash
# A: pull a public package through pull-through.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
have dotnet || { skip "dotnet not installed"; exit 0; }
D=$(dir nuget-a)
mkdir -p "$D"
cat > "$D/nuget.config" <<NX
<?xml version="1.0" encoding="utf-8"?>
<configuration>
  <packageSources>
    <add key="rucoder" value="$API/pkgs/nuget/v3/index.json" allowInsecureConnections="true" />
    <add key="nuget.org" value="https://api.nuget.org/v3/index.json" />
  </packageSources>
</configuration>
NX
cat > "$D/a.csproj" <<'CSP'
<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup><TargetFramework>net10.0</TargetFramework></PropertyGroup>
  <ItemGroup><PackageReference Include="Newtonsoft.Json" Version="13.0.3" /></ItemGroup>
</Project>
CSP
(cd "$D" && DOTNET_CLI_HOME="$D/.dotnet" dotnet restore --configfile nuget.config >/dev/null 2>&1) \
  && pass "A: dotnet restore Newtonsoft.Json via pull-through" || fail "A: dotnet restore"
