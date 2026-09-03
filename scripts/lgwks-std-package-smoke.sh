#!/usr/bin/env bash
# Verify lgwks_std stays independently packageable and consumable as an artifact.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

TARGET_DIR="${CARGO_TARGET_DIR:-target}"
if [[ "$TARGET_DIR" != /* ]]; then
  TARGET_DIR="$ROOT/$TARGET_DIR"
fi

(cd "$ROOT" && cargo package --manifest-path crates/lgwks-std/Cargo.toml --allow-dirty)

PKG_FILE="$(find "$TARGET_DIR/package" -maxdepth 1 -name 'lgwks_std-*.crate' -print | sort | tail -n 1)"
if [ -z "$PKG_FILE" ]; then
  echo "missing package artifact" >&2
  exit 1
fi

tar -xzf "$PKG_FILE" -C "$WORK"
PACKAGE_DIR="$(find "$WORK" -maxdepth 1 -mindepth 1 -type d -name 'lgwks_std-*' -print | sort | head -n 1)"
if [ -z "$PACKAGE_DIR" ]; then
  echo "missing extracted package directory" >&2
  exit 1
fi
PKG_BASENAME="${PACKAGE_DIR##*/}"

SMOKE="$WORK/consumer-smoke"
mkdir -p "$SMOKE/src"
cat >"$SMOKE/Cargo.toml" <<EOF2
[package]
name = "lgwks-std-consumer-smoke"
version = "0.0.1"
edition = "2024"

[dependencies]
lgwks_std = { path = "../${PKG_BASENAME}", features = ["random"] }
EOF2

cat >"$SMOKE/src/main.rs" <<'EOF3'
fn main() {
    let _ = lgwks_std::id::Uuid::new_v4().expect("randomness available");
    let encoded = lgwks_std::hex::encode(&[1u8, 2, 3, 4]);
    assert_eq!(encoded, "01020304");
    println!("lgwks_std package smoke passed");
}
EOF3

(cd "$SMOKE" && RUSTFLAGS='-D warnings' cargo run)
