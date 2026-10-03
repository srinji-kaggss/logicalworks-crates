#!/usr/bin/env bash
# Consume every crate this repository publishes the way a stranger would: from
# crates.io, at the exact version its newest release tag names, in a crate with
# no path into this tree. A workspace caller search is not downstream evidence
# (#155); this is.
#
# One receipt line per consumer:
#
#   <crate> tag=<tag> published=<crates.io newest> main=<version on main>
#     commits-since-tag=<n> features=<set> program=<readme|minimal> consumer=<pass|FAIL>
#
# The program is the crate's own README quickstart at that tag — the surface the
# release promised — so a README that does not compile against its own release
# fails here rather than sitting unrun. A crate whose README has no runnable
# block gets a minimal program over its public modules, and the receipt says so.
#
# Needs the network (crates.io). Exits non-zero when any consumer fails or a tag
# names a version crates.io does not serve as its newest.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="$(mktemp -d)"
dispose_work() {
  if command -v trash >/dev/null 2>&1; then
    trash "$WORK"
  elif command -v gio >/dev/null 2>&1; then
    gio trash "$WORK"
  else
    printf 'No OS Trash tool; retained consumer artifacts at %s\n' "$WORK" >&2
  fi
}
trap dispose_work EXIT
# One target directory for every consumer, so the shared dependency graph is
# compiled once rather than once per feature set.
export CARGO_TARGET_DIR="$WORK/target"

failures=0

# The newest release tag of a crate, e.g. `lgwks_std-v0.10.0`.
latest_tag() {
  git -C "$ROOT" tag --list "$1-v*" --sort=-v:refname | head -n 1
}

# The newest version crates.io serves for a crate.
published_version() {
  (cd "$WORK" && cargo info "$1" 2>/dev/null) | sed -n 's/^version: \([^ ]*\).*/\1/p' | head -n 1
}

# The first ```rust block of a crate's README at a tag, as a `main.rs`.
#
# A block that declares `fn main` is used as written. A doctest-style block is
# wrapped in a `main` returning `Result`, with rustdoc's hidden-line marker
# (`# `) removed so the hidden lines compile too.
readme_program() {
  local tag=$1 dir=$2 block
  block="$(git -C "$ROOT" show "$tag:$dir/README.md" |
    awk '/^```rust/{f=1; next} f && /^```/{exit} f{print}')"
  if [ -z "$block" ]; then
    return 1
  fi
  if printf '%s\n' "$block" | grep -q 'fn main'; then
    printf '%s\n' "$block"
    return 0
  fi
  printf 'fn main() -> Result<(), Box<dyn std::error::Error>> {\n'
  printf '%s\n' "$block" | sed -e 's/^# \{0,1\}//'
  if ! printf '%s\n' "$block" | grep -q 'Ok::<'; then
    printf 'Ok(())\n'
  fi
  printf '}\n'
}

# Build and run one consumer; prints `pass` or `FAIL`.
consume() {
  local name=$1 version=$2 spec=$3 program=$4 label=$5
  local dir="$WORK/consumer-$name-$label"
  mkdir -p "$dir/src"
  cat >"$dir/Cargo.toml" <<EOF
[package]
name = "consumer-${name//_/-}-$label"
version = "0.0.0"
edition = "2024"
publish = false

[dependencies]
$name = { version = "=$version", $spec }

[workspace]
EOF
  printf '%s\n' "$program" >"$dir/src/main.rs"
  if (cd "$dir" && cargo run --quiet >"$dir/run.log" 2>&1); then
    echo pass
  else
    sed -n '1,40p' "$dir/run.log" >&2
    echo FAIL
  fi
}

receipt() {
  local name=$1 dir=$2 label=$3 spec=$4 fallback=$5
  local tag published main_version since program result source=readme
  tag="$(latest_tag "$name")"
  published="$(published_version "$name")"
  main_version="$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/$dir/Cargo.toml" | head -n 1)"
  since="$(git -C "$ROOT" rev-list --count "$tag..HEAD")"
  if [ "${tag#"$name"-v}" != "$published" ]; then
    echo "$name: newest tag $tag but crates.io serves $published" >&2
    failures=$((failures + 1))
  fi
  if ! program="$(readme_program "$tag" "$dir")"; then
    program="$fallback"
    source=minimal
  fi
  result="$(consume "$name" "$published" "$spec" "$program" "$label")"
  if [ "$result" != pass ]; then failures=$((failures + 1)); fi
  printf '%s tag=%s published=%s main=%s commits-since-tag=%s features=%s program=%s consumer=%s\n' \
    "$name" "$tag" "$published" "$main_version" "$since" "$label" "$source" "$result"
}

STD_MINIMAL='fn main() { assert_eq!(lgwks_std::hex::encode(&[1u8, 2, 3, 4]), "01020304"); }'
DEPS_SCAN='#[allow(unused_imports)]
use lgwks_deps::{contract, lock, metadata, scan};
fn main() {}'
DEPS_TOKIO='fn main() {
    let Ok(runtime) = lgwks_deps::tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        std::process::exit(1)
    };
    runtime.block_on(async { lgwks_deps::tokio::task::yield_now().await });
}'

receipt lgwks_std crates/lgwks-std default 'default-features = true' "$STD_MINIMAL"
receipt lgwks_std crates/lgwks-std core 'default-features = false, features = ["core"]' "$STD_MINIMAL"
receipt lgwks_std crates/lgwks-std full 'features = ["full"]' "$STD_MINIMAL"
receipt lgwks_ast crates/lgwks-ast default 'default-features = true' ''
receipt lgwks_deps crates/lgwks-deps default 'default-features = true' "$DEPS_SCAN"
receipt lgwks_deps crates/lgwks-deps tokio-full 'default-features = false, features = ["tokio-full"]' "$DEPS_TOKIO"
receipt lgwks_bot crates/lgwks-bot default 'default-features = true' ''
receipt lgwks_bot crates/lgwks-bot full 'features = ["full"]' ''

if [ "$failures" -gt 0 ]; then
  echo "published consumers: $failures failure(s)" >&2
  exit 1
fi
echo "published consumers: all passed"
