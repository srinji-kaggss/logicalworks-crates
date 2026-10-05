#!/usr/bin/env bash
# The cross-target lane: one `cargo check` per declared target, per crate.
#
# A portability claim is an executed build, not a `cfg` read. Every target below
# is one this repository declares support for, and each is checked by running
# `cargo check --locked --target <t>` rather than by inference, because the
# difference between "the module is behind a cfg" and "the module compiles for
# this target" is exactly the difference #276 was filed about: `random` carried
# a three-target `compile_error!` while the backend it wraps supported dozens
# more.
#
# Two classes of check, because two different things are being claimed:
#
#   required  the estate's own Rust-only surface, which must compile for every
#             declared target. A failure here is a regression and fails the lane.
#   recorded  a target that needs a C toolchain this runner does not have. The
#             check runs, its result is printed with the exact error, and it is
#             only allowed to fail for the declared reason — a missing or
#             foreign C compiler in a vendored `cc`-built dependency. Any other
#             failure fails the lane, so the exemption cannot become a lid.
#
# Usage: scripts/check-target-matrix.sh [TARGET ...]
#
# With no arguments it checks every target below plus the host triple.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# `RUSTFLAGS=""` states the flags a cross-target build has: none. Without it a
# machine's `$CARGO_HOME/config.toml` `[build] rustflags` reach the WASI target,
# which has no rustflags table of its own, and nightly-only `-Z` flags there make
# the pinned stable rustc refuse to start. The variable replaces config rustflags
# rather than joining them. (#195)
export RUSTFLAGS=""

# The declared target set. These are the targets `lgwks_std --features full` is
# expected to build for; every one of them is exercised by the recorded check,
# and the required checks below cover what every target must build. Arguments
# replace the list, so a contributor can run one target while iterating.
DEFAULT_TARGETS=(
  aarch64-unknown-linux-gnu
  x86_64-unknown-linux-musl
  wasm32-wasip1
  x86_64-unknown-freebsd
  aarch64-linux-android
  aarch64-apple-ios
)

# The failures the recorded checks are allowed to report. Each names a C
# toolchain this runner does not have for a dependency whose build script
# compiles C per target: `ring` (through rustls, under `http`) and the
# tree-sitter grammars (under `lgwks_ast`). Neither is estate source.
#
# The test is deliberately two-part. A bare `cc-rs` would exempt any C compile
# failure, including one caused by estate source; requiring the toolchain
# phrases as well keeps the exemption to "this runner cannot reach that
# target's C compiler".
cc_missing_tool='failed to find tool'
cc_cannot_target='did not execute successfully'
cc_prefix='error occurred in cc-rs'

# One per check: pass, exempt or fail, as 0, 2 or 1.
statuses=()
# One per check: the wall time in whole seconds.
durations=()
# One per check: what the check was.
labels=()

# Records one check's outcome and its label. `pass` is a build that compiled,
# `exempt` is a recorded check this runner cannot reach for the declared C
# toolchain reason, and `FAIL` is anything else.
record() {
  local label="$1" started="$2" output="$3" status="$4"
  local now
  now="$(date +%s)"
  labels+=("$label")
  durations+=("$((now - started))")
  statuses+=("$status")
  case "$status" in
    0) printf '  pass   %4ss  %s\n' "$((now - started))" "$label" ;;
    2)
      printf '  exempt %4ss  %s\n' "$((now - started))" "$label"
      printf '%s\n' "$output" | grep -E '^error' | head -2 | sed 's/^/          /'
      ;;
    *)
      printf '  FAIL   %4ss  %s\n' "$((now - started))" "$label"
      printf '%s\n' "$output" | grep -E '^error' | head -3 | sed 's/^/          /'
      ;;
  esac
}

# True when a failing output is one of the declared C-toolchain exemptions.
# A failure that is not exempt is a real one, and the caller treats it as fatal.
is_exempt_toolchain_failure() {
  local output="$1"
  case "$output" in
    *"$cc_prefix"*) ;;
    *) return 1 ;;
  esac
  case "$output" in
    *"$cc_missing_tool"*|*"$cc_cannot_target"*) return 0 ;;
    *) return 1 ;;
  esac
}

# Runs one required check: any failure fails the lane.
required_check() {
  local target="$1"
  shift
  local output status started
  started="$(date +%s)"
  output="$("$@" 2>&1)"
  status=$?
  record "$target :: $*" "$started" "$output" "$status"
  if [ "$status" -ne 0 ]; then
    printf '\n%s\n' "$output" | tail -25
    return 1
  fi
  return 0
}

# Runs one recorded check: a failure is fatal unless it is the declared
# C-toolchain exemption, which is printed with its reason either way.
recorded_check() {
  local target="$1"
  shift
  local output status started
  started="$(date +%s)"
  output="$("$@" 2>&1)"
  status=$?
  if [ "$status" -ne 0 ] && is_exempt_toolchain_failure "$output"; then
    record "$target :: $*" "$started" "$output" 2
    printf '          not exercised here: no C toolchain for %s; a new failure on this line is a regression\n' "$target"
    return 0
  fi
  record "$target :: $*" "$started" "$output" "$status"
  if [ "$status" -ne 0 ]; then
    printf '\n%s\n' "$output" | tail -25
    return 1
  fi
  return 0
}

host_triple() {
  rustc -vV | awk '/^host:/ { print $2 }'
}

ensure_target() {
  local target="$1" output status
  if rustup target list --installed | grep -qx "$target"; then
    return 0
  fi
  output="$(rustup target add "$target" 2>&1)"
  status=$?
  if [ "$status" -ne 0 ]; then
    printf '  FAIL  rustup could not install %s; the declared target is unreachable here\n' "$target"
    printf '%s\n' "$output" | tail -5 | sed 's/^/        /'
    return 1
  fi
  printf '  added %s\n' "$target"
  return 0
}

if [ "$#" -gt 0 ]; then
  TARGETS=("$@")
else
  TARGETS=("${DEFAULT_TARGETS[@]}")
fi

started_total="$(date +%s)"
failures=0
printf 'cross-target matrix: host %s, rustc %s\n' "$(host_triple)" "$(rustc --version)"
printf 'targets: %s %s\n' "${TARGETS[*]}" "$(host_triple)"

for target in "${TARGETS[@]}" "$(host_triple)"; do
  printf '\n=== %s\n' "$target"
  ensure_target "$target" || {
    failures=$((failures + 1))
    continue
  }
  # Required: the Rust-only surface, on every target.
  required_check "$target" cargo check --locked -p lgwks_std --target "$target" \
    || failures=$((failures + 1))
  required_check "$target" cargo check --locked -p lgwks_std --no-default-features --target "$target" \
    || failures=$((failures + 1))
  # The portability claim this lane exists for: `random` builds wherever the
  # backend it wraps has an entropy source, which is every target getrandom
  # supports.
  required_check "$target" cargo check --locked -p lgwks_std --no-default-features --features random \
    --target "$target" \
    || failures=$((failures + 1))
  required_check "$target" cargo check --locked -p lgwks_deps --target "$target" \
    || failures=$((failures + 1))
  # Recorded: everything else, and the reasons a runner may not reach it.
  recorded_check "$target" cargo check --locked -p lgwks_std --features full --target "$target" \
    || failures=$((failures + 1))
  recorded_check "$target" cargo check --locked -p lgwks_ast --target "$target" \
    || failures=$((failures + 1))
done

printf '\n--- summary\n'
exempt=0
index=0
while [ "$index" -lt "${#labels[@]}" ]; do
  case "${statuses[$index]}" in
    0) name=pass ;;
    2)
      name=exempt
      exempt=$((exempt + 1))
      ;;
    *) name=FAIL ;;
  esac
  printf '%-6s %4ss %s\n' "$name" "${durations[$index]}" "${labels[$index]}"
  index=$((index + 1))
done
printf '\ntotal wall %ss across %s checks (%s exempt for a missing C toolchain)\n' \
  "$(( $(date +%s) - started_total ))" "${#labels[@]}" "$exempt"

if [ "$failures" -ne 0 ]; then
  printf '\ntarget matrix failed: %s required or unexpected failure(s)\n' "$failures" >&2
  exit 1
fi
printf '\ntarget matrix passed\n'