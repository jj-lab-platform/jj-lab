#!/usr/bin/env bash
# B: publish a self-made crate.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
have cargo || { skip "cargo not installed"; exit 0; }
NAME="e2e-cargo-$RUN_ID"
echo "$NAME" > "$WORK/cargo-pkg-name"
D=$(dir cargo-b)
mkdir -p "$D/src"
cat > "$D/Cargo.toml" <<T
[package]
name = "$NAME"
version = "1.0.0"
edition = "2021"
T
echo 'pub fn hello()->&'"'"'static str{"hello"}' > "$D/src/lib.rs"
(cd "$D" && CARGO_HOME="$D/.ch" cargo package -q --allow-dirty --no-verify >/dev/null 2>&1) && pass "B: cargo package" || fail "B: cargo package"
CRATE="$D/target/package/$NAME-1.0.0.crate"
VERS="1.0.0"
python3 - "$NAME" "$VERS" "$CRATE" > /tmp/cargo-pubbody <<'PY'
import sys, struct, json, pathlib
name, ver, crate = sys.argv[1], sys.argv[2], sys.argv[3]
meta = json.dumps({"name": name, "vers": ver, "deps": [], "features": {}, "authors": []}).encode()
data = pathlib.Path(crate).read_bytes()
sys.stdout.buffer.write(struct.pack('<I', len(meta)) + meta + struct.pack('<I', len(data)) + data)
PY
code=$(curl_api -s -o /dev/null -w '%{http_code}' -X PUT --data-binary "@$(echo /tmp/cargo-pubbody)" "$API/pkgs/cargo/api/v1/crates/new")
assert_eq "B: cargo publish $NAME" 201 "$code"
