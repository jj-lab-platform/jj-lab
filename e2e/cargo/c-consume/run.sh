#!/usr/bin/env bash
# C: consume the crate pushed in B.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
have cargo || { skip "cargo not installed"; exit 0; }
NAME=$(cat "$WORK/cargo-pkg-name")
MOD="${NAME//-/_}"
D=$(dir cargo-c)
mkdir -p "$D/src" "$D/.cargo"
cat > "$D/.cargo/config.toml" <<C
[registries]
rucoder = { index = "sparse+$API/pkgs/cargo/" }
C
cat > "$D/Cargo.toml" <<T
[package]
name = "cargo-c"
version = "0.1.0"
edition = "2021"
[dependencies]
$NAME = { version = "1.0.0", registry = "rucoder" }
T
echo "fn main(){assert_eq!($MOD::hello(),\"hello\");}" > "$D/src/main.rs"
(cd "$D" && CARGO_HOME="$D/.ch" cargo run -q >/dev/null 2>&1) && pass "C: cargo run depends on $NAME (just pushed)" || fail "C: cargo run $NAME"
