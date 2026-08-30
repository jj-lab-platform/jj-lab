#!/usr/bin/env bash
# F: republish — same crate/version with different bytes; index cksum must
# match the new crate (not the first publish's).
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
have cargo || { skip "cargo not installed"; exit 0; }
NAME=$(cat "$WORK/cargo-pkg-name")
D=$(dir cargo-f)
mkdir -p "$D/src"

cat > "$D/Cargo.toml" <<T
[package]
name = "$NAME"
version = "1.0.0"
edition = "2021"
T
echo 'pub fn hello()->&'"'"'static str{"overwritten"}' > "$D/src/lib.rs"
(cd "$D" && CARGO_HOME="$D/.ch" cargo package -q --allow-dirty --no-verify >/dev/null 2>&1)
CRATE="$D/target/package/$NAME-1.0.0.crate"
[ -f "$CRATE" ] || { fail "F: crate not produced"; exit 0; }
SUM=$(sha256sum "$CRATE" | cut -d' ' -f1)

python3 - "$NAME" "1.0.0" "$CRATE" > /tmp/cargo-pubbody-f <<'PY'
import sys, struct, json, pathlib
name, ver, crate = sys.argv[1], sys.argv[2], sys.argv[3]
meta = json.dumps({"name": name, "vers": ver, "deps": [], "features": {}, "authors": []}).encode()
data = pathlib.Path(crate).read_bytes()
sys.stdout.buffer.write(struct.pack('<I', len(meta)) + meta + struct.pack('<I', len(data)) + data)
PY
code=$(curl_api -s -o /dev/null -w '%{http_code}' -X PUT --data-binary "@/tmp/cargo-pubbody-f" "$API/pkgs/cargo/api/v1/crates/new")
assert_eq "F: cargo republish" 201 "$code"

out=$(curl_api "$API/pkgs/cargo/$NAME")
echo "$out" | python3 -c "import json,sys; lines=[json.loads(l) for l in sys.stdin.read().splitlines() if l.strip()]; [print(l.get('cksum')) for l in lines if l.get('vers')=='1.0.0']" | grep -q "$SUM" \
  && pass "F: index cksum refreshed" || fail "F: index cksum refreshed"