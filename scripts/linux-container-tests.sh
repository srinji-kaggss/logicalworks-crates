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
# The container engine on macOS is OrbStack (Director, 2026-10-07): Docker
# Desktop is not installed on the CI machine, and the script refuses any other
# engine there rather than run the Linux leg on whichever one `docker` happens
# to reach. OrbStack serves the Docker API, so the commands below are the
# Docker CLI's. On Linux the engine is the host's own.
#
# The image is the repository's pinned toolchain (`rust-toolchain.toml`) and
# nextest is the version the host runs, so the two legs differ only in the OS.
# Build output, the registry and the nextest binary live in named volumes, so a
# run reuses what the last run built.
#
# `vps` is not a test suite: it runs the bench/async matrix under the 2-vCPU /
# 2 GB profile the hyperscale axis names (#269). It builds uncapped, then runs
# the built binary in a second container capped by cgroup (`--cpus=2
# --memory=2g`, no swap), and writes each record under bench/async/vps/. It
# runs alone, on a quiet host: a capped run beside other jobs measures them.
#
# Usage: scripts/linux-container-tests.sh [workspace|bot-full|appcui]...  (default: all three)
#        scripts/linux-container-tests.sh vps
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

# The engine is named by its socket rather than by the current Docker context:
# a context is per-user client state that another tool can switch, and a run
# whose engine depended on it found none at all (run 37619560219 attempt 2).
# The CLI is OrbStack's own, from its app bundle: the runners' PATH reached a
# `docker` only through Homebrew's Docker formula, which is removed with the
# rest of Docker (run 37623747480: `docker: command not found`).
if [ "$(uname -s)" = Darwin ]; then
    export PATH="/Applications/OrbStack.app/Contents/MacOS/xbin:${HOME}/.orbstack/bin:${PATH}"
    export DOCKER_HOST="unix://${HOME}/.orbstack/run/docker.sock"
    if ! engine="$(docker info --format '{{.OperatingSystem}}' 2>&1)" || [ "${engine}" != OrbStack ]; then
        echo "linux-container-tests: no OrbStack engine at ${DOCKER_HOST}: ${engine}" >&2
        echo "linux-container-tests: start OrbStack (\`orb start\`); the Linux leg runs on OrbStack only" >&2
        exit 2
    fi
fi

suites=("$@")
if [ "${#suites[@]}" -eq 0 ]; then
    suites=(workspace bot-full appcui)
fi
commands=()
# The matrix the VPS profile runs: every bench/async door the M5 Pro tables
# report, each writing its own record. Run inside the capped container.
vps_bench=/target/release/lgwks-bench-async
vps_out=bench/async/vps
vps_commands=(
    "mkdir -p ${vps_out}"
    "nproc; grep -E '^(MemTotal|MemAvailable)' /proc/meminfo; cat /sys/fs/cgroup/cpu.max /sys/fs/cgroup/memory.max"
    "${vps_bench} --rounds=15 --alloc-report --json=${vps_out}/results.json"
    "${vps_bench} --tiers"
    "${vps_bench} --matrix --json=${vps_out}/matrix.json"
    "${vps_bench} --flood-split --rounds=41"
    "${vps_bench} --saturation --window=1 --json=${vps_out}/saturation-backpressure.json"
    "${vps_bench} --refusal --window=1 --json=${vps_out}/saturation-refuse.json"
    "${vps_bench} --overload --bound=64 --knee=10000 --baseline-seconds=5 --overload-seconds=30 --recovery-seconds=90 --json=${vps_out}/overload.json"
    # Last, because its widest tier may exceed the 2 GB cap: an OOM kill
    # (exit 137) is the cap reached, and it is reported as that.
    "${vps_bench} --inflight --json=${vps_out}/inflight.json || echo \"inflight: exit \$? under the 2 GB cap\""
)
if [ "${#suites[@]}" -gt 1 ]; then
    for suite in "${suites[@]}"; do
        if [ "${suite}" = vps ]; then
            echo "linux-container-tests: vps runs alone; a capped run beside a suite measures the suite" >&2
            exit 2
        fi
    done
fi
for suite in "${suites[@]}"; do
    case "${suite}" in
        vps) commands+=("cargo build --locked --release --manifest-path bench/async/Cargo.toml") ;;
        workspace) commands+=("cargo nextest run --workspace --locked -E 'not binary(storefront_consumers)'") ;;
        bot-full) commands+=("cargo nextest run -p lgwks_bot --all-targets --locked --features full -E 'not test(saturation_r32_tier)'") ;;
        appcui) commands+=(
            "cargo test --locked -p lgwks_deps --no-default-features --features appcui --lib"
            "cargo test --locked -p lgwks_deps --no-default-features --features appcui --doc"
            "cargo clippy --locked -p lgwks_deps --no-default-features --features appcui --all-targets -- -D warnings"
        ) ;;
        *) echo "linux-container-tests: unknown suite ${suite} (workspace, bot-full, appcui, vps)" >&2; exit 2 ;;
    esac
done

script="set -euo pipefail
# A job killed mid-unpack (a timeout on a transition run) leaves a package
# directory with no .cargo-ok, and the next unpack of that package fails
# with 'File exists' (run 37673046041: tree-sitter-nix). Cargo never repairs
# one, so this removes the partials before anything builds: a directory with
# its .cargo-ok is a complete unpack and is kept, anything else under the
# sources is re-unpacked from the cached .crate file. No backquotes in this
# string: it is double-quoted, so the host shell would run them as commands.
for sources in /usr/local/cargo/registry/src/*/; do
    [ -d \"\${sources}\" ] || continue
    find \"\${sources}\" -mindepth 1 -maxdepth 1 -type d ! -exec test -e '{}/.cargo-ok' ';' -print | while IFS= read -r partial; do
        echo \"linux-container-tests: removing partial unpack \${partial}\"
        rm -rf \"\${partial}\"
    done
done
if [ \"\$(/opt/tools/cargo-nextest --version 2>/dev/null | awk 'NR==1 {print \$2}')\" != '${nextest}' ]; then
    curl -fsSL 'https://get.nexte.st/${nextest}/${nextest_platform}' | tar -xz -C /opt/tools
fi
case ' ${container_rustflags} ' in
    *fuse-ld=lld*) command -v ld.lld >/dev/null || { apt-get update -qq && apt-get install -y -qq lld >/dev/null; } ;;
esac
export PATH=/opt/tools:\$PATH
rustc --version
$(printf '%s\n' "${commands[@]}")"

# The target volume belongs to one runner. Every runner on this machine talks
# to the same OrbStack engine, so a volume named only by its suites was shared by
# every concurrent run of that suite: one run relinked a test binary while
# another was executing it, and nextest's exec failed with `No such file or
# directory` partway through the bot-full suite. A runner runs one job at a
# time, so its own volume has one writer. Outside CI the owner is `local`.
target_owner="$(printf '%s' "${RUNNER_NAME:-local}" | tr -c 'A-Za-z0-9_.-' '-')"

# `--init` puts a reaping init at PID 1, as a Linux host has. Without it the
# shell is PID 1, never reaps the descendants a supervised process orphans, and
# a killed descendant stays a zombie that `kill(pid, 0)` still reports present.
#
# `/tmp` is a tmpfs, for the reason the macOS jobs keep theirs on a RAM disk
# (WORKFLOW.md §12): the durable-store tests `fsync` every record, and in the
# VM each one reaches the host's disk through the virtual block device. No test
# simulates power loss, so no assertion depends on where the bytes land.
# Docker's tmpfs default is `noexec`; the suites execute scripts they write
# there (the fake `gh`), so the mount says `exec`. The size is a ceiling, not a
# reservation: a tmpfs holds only what is written to it, and it dies with the
# container.
run=(docker run --rm --init
    --volume "${root}:/src"
    --volume "lwc-ci-linux-target-${target_owner}-$(IFS=-; echo "${suites[*]}"):/target"
    --volume lwc-ci-linux-registry:/usr/local/cargo/registry
    --volume lwc-ci-linux-tools:/opt/tools
    ${config_mount[@]+"${config_mount[@]}"}
    --env "${rustflags_var}=${container_rustflags}"
    --env CARGO_TARGET_DIR=/target
    --env NEXTEST_TEST_THREADS
    --env CARGO_PROFILE_TEST_DEBUG="${CARGO_PROFILE_TEST_DEBUG:-line-tables-only}"
    --env RUSTC_WORKSPACE_WRAPPER=
    --workdir /src)
image="rust:${toolchain}-bookworm"
if [ "${suites[*]}" != vps ]; then
    exec "${run[@]}" --tmpfs /tmp:rw,exec,nosuid,size=3g "${image}" bash -c "${script}"
fi
# The build is uncapped, so the cap measures the run and not the compile. The
# run's `/tmp` is a 64 MiB tmpfs: tmpfs pages count against the container's
# memory cgroup, and the matrix writes nothing there.
"${run[@]}" --tmpfs /tmp:rw,exec,nosuid,size=3g "${image}" bash -c "${script}"
exec "${run[@]}" --tmpfs /tmp:rw,exec,nosuid,size=64m \
    --cpus=2 --memory=2g --memory-swap=2g "${image}" \
    bash -c "set -euo pipefail
$(printf '%s\n' "${vps_commands[@]}")"
