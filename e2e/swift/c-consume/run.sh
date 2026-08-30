#!/usr/bin/env bash
# C: consume the published package via swift package resolve + build.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
have swift || { skip "swift not installed"; exit 0; }

P=$(cat "$WORK/swift-pkg-dir" 2>/dev/null)
[ -n "$P" ] || { skip "B did not run"; exit 0; }
REG=$(cat "$WORK/swift-reg" 2>/dev/null)

D=$(dir swift-c)
CFG="$D/.swiftpm-cfg"; SEC="$D/.swiftpm-sec"; CCH="$D/.swiftpm-cache"
mkdir -p "$CFG" "$SEC" "$CCH"
export HOME="$D/.home"
mkdir -p "$HOME"

C="$D/consumer"
mkdir -p "$C/Sources/Consumer"
cat > "$C/Package.swift" <<SW
// swift-tools-version:5.9
import PackageDescription
let package = Package(
    name: "Consumer",
    dependencies: [
        .package(id: "xcliorg.xclikit", exact: "1.0.0")
    ],
    targets: [
        .executableTarget(
            name: "Consumer",
            dependencies: [.product(name: "XcliKit", package: "xcliorg.xclikit")]
        )
    ]
)
SW
cat > "$C/Sources/Consumer/main.swift" <<'SW'
import XcliKit
print(XcliKit.v())
SW
(cd "$C" && swift package-registry set --config-path "$CFG" --security-path "$SEC" --cache-path "$CCH" "$REG" --scope xcliorg >/dev/null 2>&1)
(cd "$C" && swift package-registry login "$REG" --token "$WRITE_TOKEN" --no-confirm --config-path "$CFG" --security-path "$SEC" --cache-path "$CCH" >/dev/null 2>&1)
out=$(cd "$C" && timeout 300 swift package resolve --config-path "$CFG" --security-path "$SEC" --cache-path "$CCH" --scratch-path "$C/.build" 2>&1); rc=$?
if [ $rc -eq 0 ]; then
  pass "C: swift package resolve"
else
  fail "C: swift package resolve: $(echo "$out" | tail -2)"
fi
if (cd "$C" && timeout 300 swift build --config-path "$CFG" --security-path "$SEC" --cache-path "$CCH" --scratch-path "$C/.build" >/dev/null 2>&1 && [ "$("$C/.build/debug/Consumer" 2>/dev/null)" = "9" ]); then
  pass "C: swift build executes"
else
  fail "C: swift build executes"
fi
