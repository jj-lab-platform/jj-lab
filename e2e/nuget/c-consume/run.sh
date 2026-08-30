#!/usr/bin/env bash
# C: consume the package pushed in B, from C# AND F#.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
have dotnet || { skip "dotnet not installed"; exit 0; }
ID=$(cat "$WORK/nuget-name")
mkcfg() { cat > "$1/nuget.config" <<NX
<?xml version="1.0" encoding="utf-8"?>
<configuration>
  <packageSources>
    <add key="rucoder" value="$API/pkgs/nuget/v3/index.json" allowInsecureConnections="true" />
    <add key="nuget.org" value="https://api.nuget.org/v3/index.json" />
  </packageSources>
</configuration>
NX
}

# C#
D=$(dir nuget-c)
mkdir -p "$D"
mkcfg "$D"
cat > "$D/c.csproj" <<CSP
<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup><TargetFramework>net10.0</TargetFramework></PropertyGroup>
  <ItemGroup><PackageReference Include="$ID" Version="1.0.0" /></ItemGroup>
</Project>
CSP
(cd "$D" && DOTNET_CLI_HOME="$D/.dotnet" dotnet restore --configfile nuget.config >/dev/null 2>&1) \
  && pass "C(c#): dotnet restore $ID" || fail "C(c#): dotnet restore $ID"

# F#
D=$(dir nuget-c-fsharp)
mkdir -p "$D"
mkcfg "$D"
cat > "$D/c.fsproj" <<CSP
<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup><TargetFramework>net10.0</TargetFramework></PropertyGroup>
  <ItemGroup><Compile Include="Lib.fs" /></ItemGroup>
  <ItemGroup><PackageReference Include="$ID" Version="1.0.0" /></ItemGroup>
</Project>
CSP
echo 'module X' > "$D/Lib.fs"
(cd "$D" && DOTNET_CLI_HOME="$D/.dotnet" dotnet restore --configfile nuget.config >/dev/null 2>&1) \
  && pass "C(f#): dotnet restore $ID" || fail "C(f#): dotnet restore $ID"
