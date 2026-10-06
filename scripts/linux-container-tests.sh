#!/usr/bin/env bash
# The Linux leg of the gate, run in a container on the self-hosted macOS runner.
#
# CI runs on the local runner (Director, 2026-10-04), which is macOS on arm64.
# The supervisor and lgwks_std::process have Linux-only paths (`/proc` child
# lists, the Linux reap), so a macOS-only gate would compile them and run none.
# This runs the suites that exercise them on Linux inside a container (the
# workspace suite, the lgwks-bot full-feature suite and the AppCUI storefront),
# so every Linux path still executes on every run. Each suite set has its own
# target volume, so CI can run the suites side by side without one waiting on
# another's build lock.
#
# The image is the repository's pinned toolchain (`rust-toolchain.toml`) and
# nextest is the version the host runs, so the two legs differ only in the OS.
# Build output, the registry and the nextest binary live in named volumes, so a
# run reuses what the last run built.
#
# Usage: scripts/linux-container-tests.sh [workspace|bot-full|appcui]...  (default: all three)
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
toolchain="$(python3 -c 'import sys,tomllib; print(tomllib.load(open(sys.argv[1],"rb"))["toolchain"]["channel"])' "${root}/rust-toolchain.toml")"
nextest="$(cargo nextest --version | awk 'NR==1 {print $2}')"
case "$(uname -m)" in
    arm64 | aarch64) nextest_platform="linux-arm" ;;
    x86_64) nextest_platform="linux" ;;
    *) echo "linux-container-tests: no nextest build for $(uname -m)" >&2; exit 2 ;;
esac

# The machine's cargo configuration is the build (Director, 2026-10-06: CI
# uses every optimisation the global cargo config declares). The container
# reads the same file, and the host target's rustflags (lld, target-cpu=native)
# are applied to the container's target, so its own target table takes
# precedence over `[build] rustflags` exactly as the host's does.
cargo_home="${CARGO_HOME:-${HOME}/.cargo}"
host_triple="$(rustc -vV | awk '/^host:/ {print $2}')"
container_triple="$(uname -m | sed 's/^arm64$/aarch64/')-unknown-linux-gnu"
container_rustflags="$(python3 -c 'import sys,tomllib
try:
    config = tomllib.load(open(sys.argv[1], "rb"))
except FileNotFoundError:
    config = {}
print(" ".join(config.get("target", {}).get(sys.argv[2], {}).get("rustflags", [])))' "${cargo_home}/config.toml" "${host_triple}")"
rustflags_var="CARGO_TARGET_$(echo "${container_triple}" | tr 'a-z-' 'A-Z_')_RUSTFLAGS"
config_mount=()
if [ -f "${cargo_home}/config.toml" ]; then
    config_mount=(--volume "${cargo_home}/config.toml:/usr/local/cargo/config.toml:ro")
fi

suites=("$@")
if [ "${#suites[@]}" -eq 0 ]; then
    suites=(workspace bot-full appcui)
fi
commands=()
for suite in "${suites[@]}"; do
    case "${suite}" in
        workspace) commands+=("cargo nextest run --workspace --locked -E 'not binary(storefront_consumers)'") ;;
        bot-full) commands+=("cargo nextest run -p lgwks_bot --all-targets --locked --features full -E 'not test(saturation_r32_tier)'") ;;
        appcui) commands+=(
            "cargo test --locked -p lgwks_deps --no-default-features --features appcui --lib"
            "cargo test --locked -p lgwks_deps --no-default-features --features appcui --doc"
            "cargo clippy --locked -p lgwks_deps --no-default-features --features appcui --all-targets -- -D warnings"
        ) ;;
        *) echo "linux-container-tests: unknown suite ${suite} (workspace, bot-full, appcui)" >&2; exit 2 ;;
    esac
done

script="set -euo pipefail
if [ \"\$(/opt/tools/cargo-nextest --version 2>/dev/null | awk 'NR==1 {print \$2}')\" != '${nextest}' ]; then
    curl -fsSL 'https://get.nexte.st/${nextest}/${nextest_platform}' | tar -xz -C /opt/tools
fi
case ' ${container_rustflags} ' in
    *fuse-ld=lld*) command -v ld.lld >/dev/null || { apt-get update -qq && apt-get install -y -qq lld >/dev/null; } ;;
esac
export PATH=/opt/tools:\$PATH
rustc --version
$(printf '%s\n' "${commands[@]}")"

# `--init` puts a reaping init at PID 1, as a Linux host has. Without it the
# shell is PID 1, never reaps the descendants a supervised process orphans, and
# a killed descendant stays a zombie that `kill(pid, 0)` still reports present.
exec docker run --rm --init \
    --volume "${root}:/src" \
    --volume "lwc-ci-linux-target-$(IFS=-; echo "${suites[*]}"):/target" \
    --volume lwc-ci-linux-registry:/usr/local/cargo/registry \
    --volume lwc-ci-linux-tools:/opt/tools \
    ${config_mount[@]+"${config_mount[@]}"} \
    --env "${rustflags_var}=${container_rustflags}" \
    --env CARGO_TARGET_DIR=/target \
    --env NEXTEST_TEST_THREADS \
    --env CARGO_PROFILE_TEST_DEBUG="${CARGO_PROFILE_TEST_DEBUG:-line-tables-only}" \
    --env RUSTC_WORKSPACE_WRAPPER= \
    --workdir /src \
    "rust:${toolchain}-bookworm" \
    bash -c "${script}"
