#!/usr/bin/env bash
# A: pull a public crate through pull-through.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
have cargo || { skip "cargo not installed"; exit 0; }
D=$(dir cargo-a)
mkdir -p "$D/src" "$D/.cargo"
cat > "$D/Cargo.toml" <<'T'
[package]
name = "cargo-a"
version = "0.1.0"
edition = "2021"
[dependencies]
anyhow = "1.0"
T
echo 'fn main(){anyhow::Result::<()>::Ok(()).unwrap();}' > "$D/src/main.rs"
cat > "$D/.cargo/config.toml" <<C
[source.crates-io]
replace-with = "rucoder"
[source.rucoder]
registry = "sparse+$API/pkgs/cargo/"
C
(cd "$D" && CARGO_HOME="$D/.ch" cargo check -q >/dev/null 2>&1) && pass "A: cargo check anyhow via pull-through" || fail "A: cargo check anyhow"
