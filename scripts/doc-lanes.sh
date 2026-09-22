#!/usr/bin/env bash
# The rustdoc gate, as one command.
#
# A broken intra-doc link is a warning, not an error, so nothing fails until
# `RUSTDOCFLAGS='-D warnings'` turns it into one. Which links break then depends
# on which features are on, because a link resolves or dangles per feature set.
# No single build is the whole check, so the gate is a set of lanes rather than
# one command, and every lane below has already caught a real break here.
#
# Usage: scripts/doc-lanes.sh [LANE ...]     (no arguments runs every lane)
#
#   all-features         every crate with every feature on
#   no-default-features  every crate with no features on
#   default              every crate at the set `cargo add` hands a consumer
#   per-feature          lgwks_std with each of its features alone
#   all                  every lane above
#
# This script is the one copy of those commands. Both the Docs CI job and the
# `Checks that must pass` list in AGENTS.md call it, so a contributor runs the
# same gate CI runs instead of a shorter one that reports green.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# Every invocation carries this, and it is the whole point of the script:
# without it a dangling link is a warning and the exit status is still 0.
export RUSTDOCFLAGS='-D warnings'

doc() {
  printf '\n--- cargo doc %s\n' "$*"
  cargo doc --no-deps --locked "$@"
}

# Every feature on, per crate. This lane resolves a link into a feature-gated
# module, so a link from a default-on module to a default-off one passes here
# and still breaks for every consumer: `trace` (default-on) linked to `json`
# (default-off), and the repository did not see it.
#
# `lgwks_deps` is built with `--features tokio-full` rather than
# `--all-features`, because `gpui` pulls `objc2`, which refuses to compile off
# Apple targets. The gpui renderer is covered by the gpui-macos CI job instead.
lane_all_features() {
  doc -p lgwks_std --all-features
  doc -p lgwks_bot --all-features
  doc -p lgwks_ast --all-features
  doc -p lgwks_deps --features tokio-full
}

# No features on, per crate. The other boundary: this lane does not compile the
# module that holds such a link at all, which is why it and the default lane are
# both needed. `retry` linked to `random` and only this lane could see it.
lane_no_default_features() {
  doc --manifest-path crates/lgwks-std/Cargo.toml --no-default-features
  doc --manifest-path crates/lgwks-bot/Cargo.toml --no-default-features
  doc --manifest-path crates/lgwks-ast/Cargo.toml --no-default-features
}

# The set `cargo add lgwks_std` actually hands a consumer, which neither lane
# above builds: `--all-features` resolves a link into a gated module, and
# `--no-default-features` omits the module that holds it. A break here is one
# every consumer sees and the repository does not.
lane_default() {
  doc -p lgwks_std
  doc -p lgwks_bot
  doc -p lgwks_ast
}

# Each feature alone. This is the general case the two boundary lanes cannot
# reach: a link between two optional modules where neither implies the other
# resolves under every combination of `--all-features` and
# `--no-default-features`. `wire` linked to `json`, and only this lane can see
# it. Documenting each feature alone means every gated module is documented
# while every module it does not pull in is genuinely absent.
lane_per_feature() {
  local features
  features="$(python3 - <<'PY'
import tomllib

with open("crates/lgwks-std/Cargo.toml", "rb") as handle:
    parsed = tomllib.load(handle)
names = [name for name in parsed["features"] if name not in ("default", "full")]
print(" ".join(sorted(names)))
PY
)"
  printf '\nlgwks-std features: %s\n' "$features"
  local feature
  for feature in $features; do
    doc -p lgwks_std --no-default-features --features "$feature"
  done
}

run_lane() {
  case "$1" in
    all-features) lane_all_features ;;
    no-default-features) lane_no_default_features ;;
    default) lane_default ;;
    per-feature) lane_per_feature ;;
    all)
      lane_all_features
      lane_no_default_features
      lane_default
      lane_per_feature
      ;;
    *)
      printf 'unknown lane: %s\n' "$1" >&2
      printf 'lanes: all-features no-default-features default per-feature all\n' >&2
      exit 2
      ;;
  esac
}

if [ "$#" -eq 0 ]; then
  set -- all
fi

for lane in "$@"; do
  printf '\n=== %s\n' "$lane"
  run_lane "$lane"
done

printf '\nall requested doc lanes passed\n'
