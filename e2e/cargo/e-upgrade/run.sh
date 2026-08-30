#!/usr/bin/env bash
# E: upgrade — publish 1.0.1, assert sparse index lists it.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
have cargo || { skip "cargo not installed"; exit 0; }
NAME=$(cat "$WORK/cargo-pkg-name")
D=$(dir cargo-e)
mkdir -p "$D/src"
cat > "$D/Cargo.toml" <<T
[package]
name = "$NAME"
version = "1.0.1"
edition = "2021"
T
echo 'pub fn hello()->&'"'"'static str{"hello2"}' > "$D/src/lib.rs"
(cd "$D" && CARGO_HOME="$D/.ch" cargo package -q --allow-dirty --no-verify >/dev/null 2>&1) && pass "E: cargo package 1.0.1" || fail "E: cargo package 1.0.1"
CRATE="$D/target/package/$NAME-1.0.1.crate"
python3 - "$NAME" "1.0.1" "$CRATE" > /tmp/cargo-pubbody2 <<'PY'
import sys, struct, json, pathlib
name, ver, crate = sys.argv[1], sys.argv[2], sys.argv[3]
meta = json.dumps({"name": name, "vers": ver, "deps": [], "features": {}, "authors": []}).encode()
data = pathlib.Path(crate).read_bytes()
sys.stdout.buffer.write(struct.pack('<I', len(meta)) + meta + struct.pack('<I', len(data)) + data)
PY
code=$(curl_api -s -o /dev/null -w '%{http_code}' -X PUT --data-binary "@/tmp/cargo-pubbody2" "$API/pkgs/cargo/api/v1/crates/new")
assert_eq "E: cargo publish 1.0.1" 201 "$code"
out=$(curl_api "$API/pkgs/cargo/$NAME")
echo "$out" | grep -q '"vers":"1.0.1"' && pass "E: sparse index lists 1.0.1" || { echo "$out" | grep -q '1.0.1' && pass "E: sparse index lists 1.0.1" || fail "E: sparse index lists 1.0.1"; }
