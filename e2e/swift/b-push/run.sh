#!/usr/bin/env bash
# B: publish via SwiftPM package-registry publish (over https ingress).
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
have swift || { skip "swift not installed"; exit 0; }

# The https ingress trusts the nip.io CA now installed system-wide. Use the
# https registry URL for both `set` and `publish`.
REG="https://jj-lab.temp.10.199.64.20.nip.io/pkgs/swift"

D=$(dir swift-b)
CFG="$D/.swiftpm-cfg"; SEC="$D/.swiftpm-sec"; CCH="$D/.swiftpm-cache"
mkdir -p "$CFG" "$SEC" "$CCH"
export HOME="$D/.home"
mkdir -p "$HOME"

swift package-registry set --config-path "$CFG" --security-path "$SEC" --cache-path "$CCH" "$REG" --scope xcliorg >/dev/null 2>&1
swift package-registry login "$REG" --token "$WRITE_TOKEN" --no-confirm --config-path "$CFG" --security-path "$SEC" --cache-path "$CCH" >/dev/null 2>&1

P="$D/xcli-kit"
mkdir -p "$P/Sources/XcliKit"
cat > "$P/Package.swift" <<'SW'
// swift-tools-version:5.9
import PackageDescription
let package = Package(
    name: "XcliKit",
    products: [.library(name: "XcliKit", targets: ["XcliKit"])],
    targets: [.target(name: "XcliKit")]
)
SW
echo 'public struct XcliKit { public static func v() -> Int { 9 } }' > "$P/Sources/XcliKit/XcliKit.swift"
echo "$P" > "$WORK/swift-pkg-dir"
echo "$REG" > "$WORK/swift-reg"

if (cd "$P" && timeout 180 swift package-registry publish --config-path "$CFG" --security-path "$SEC" --cache-path "$CCH" xcliorg.xclikit 1.0.0 --url "$REG" >/dev/null 2>&1); then
  pass "B: swift package-registry publish"
else
  fail "B: swift package-registry publish"
fi
