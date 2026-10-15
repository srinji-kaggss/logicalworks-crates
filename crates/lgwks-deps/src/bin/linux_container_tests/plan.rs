//! The choices the Linux leg makes that need no container: which image, which
//! volumes, which commands, and what the machine's cargo configuration adds.
//!
//! Every function here is a pure function of its arguments, so what a run
//! decides is checked without Docker.

use crate::toml_subset::{list, raw, read_tables, text};
use crate::{Failure, fail};

/// The suites the leg runs, in the order a bare invocation runs them.
pub(crate) const SUITES: [&str; 3] = ["workspace", "bot-full", "appcui"];

/// Where the container's build output lives: the target volume's mount, and the
/// `CARGO_TARGET_DIR` every command inside it builds into.
pub(crate) const CONTAINER_TARGET: &str = "/target";

/// The ceiling of the container's `/tmp` tmpfs when no cap names another.
const TMPFS: &str = "3g";

/// The Debian release the image is. Its util-linux 2.41 `setpriv` applies
/// Landlock rules, which `Confinement::Landlock` wraps (#337); bookworm's 2.38
/// cannot, and the Landlock simulations fail there with `Unsupported`.
const DEBIAN: &str = "trixie";

/// The packages the derived image adds to the toolchain image: `lld`, the
/// linker the machine's cargo configuration names, and `sqlite3`, the program
/// the acceptance-receipt binary holds its receipts through.
const IMAGE_PACKAGES: &str = "lld sqlite3";

/// Where the container finds the registry sources that a killed job can leave
/// half unpacked.
const REGISTRY_SOURCES: &str = "/usr/local/cargo/registry/src";

/// The toolchain channel that `rust-toolchain.toml` pins.
pub(crate) fn toolchain_channel(manifest: &str) -> Option<String> {
    read_tables(manifest)
        .iter()
        .filter(|table| !table.array && table.path == "toolchain")
        .find_map(|table| text(table, "channel").map(str::to_owned))
}

/// The rustflags under `[target.<triple>]` in a cargo configuration: an array,
/// which may wrap over lines, or the space-separated string cargo also takes.
/// A table with none gives none, and a value of any other shape is refused
/// rather than ignored, because ignoring it would build the container without
/// the machine's flags.
pub(crate) fn target_rustflags(config: &str, triple: &str) -> Result<Vec<String>, Failure> {
    let wanted = format!("target.{triple}");
    let tables = read_tables(config);
    let Some(table) = tables
        .iter()
        .find(|table| !table.array && table.path == wanted)
    else {
        return Ok(Vec::new());
    };
    if let Some(flags) = list(table, "rustflags") {
        return Ok(flags.to_vec());
    }
    if let Some(flags) = text(table, "rustflags") {
        return Ok(flags.split_whitespace().map(str::to_owned).collect());
    }
    match raw(table, "rustflags") {
        Some(found) => fail(
            format!("[{wanted}] rustflags is neither a list nor a string: {found}"),
            "target_rustflags",
        ),
        None => Ok(Vec::new()),
    }
}

/// The `host:` triple that `rustc -vV` prints.
pub(crate) fn host_triple(version: &str) -> Option<&str> {
    version
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .map(str::trim)
}

/// The version on the first line of `cargo nextest --version`
/// (`cargo-nextest 0.9.100 (abc 2026-01-01)`).
pub(crate) fn nextest_version(output: &str) -> Option<&str> {
    output.lines().next()?.split_whitespace().nth(1)
}

/// The nextest release archive that suits a machine architecture.
pub(crate) fn nextest_platform(arch: &str) -> Option<&'static str> {
    match arch {
        "aarch64" | "arm64" => Some("linux-arm"),
        "x86_64" => Some("linux"),
        _ => None,
    }
}

/// The Linux triple that the container builds for.
pub(crate) fn container_triple(arch: &str) -> String {
    let arch = if arch == "arm64" { "aarch64" } else { arch };
    format!("{arch}-unknown-linux-gnu")
}

/// The environment variable that cargo reads a triple's rustflags from.
pub(crate) fn rustflags_variable(triple: &str) -> String {
    let upper: String = triple
        .chars()
        .map(|ch| {
            if ch == '-' {
                '_'
            } else {
                ch.to_ascii_uppercase()
            }
        })
        .collect();
    format!("CARGO_TARGET_{upper}_RUSTFLAGS")
}

/// `name` with every character outside `[A-Za-z0-9_.-]` replaced by `-`, so it
/// can sit inside a Docker image tag or volume name.
pub(crate) fn slug(name: &str) -> String {
    name.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-') {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

/// The argument vector of a command that is made of fixed words.
fn words(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| (*part).to_owned()).collect()
}

/// The commands of one suite, each an argument vector, or `None` for a suite
/// the leg does not know. `partition` is nextest's `count:N/M` shard.
pub(crate) fn suite_commands(suite: &str, partition: Option<&str>) -> Option<Vec<Vec<String>>> {
    match suite {
        "workspace" => {
            let mut command = words(&[
                "cargo",
                "nextest",
                "run",
                "--workspace",
                "--locked",
                "-E",
                "not binary(storefront_consumers)",
            ]);
            if let Some(shard) = partition {
                command.extend(words(&["--partition", shard]));
            }
            Some(vec![command])
        }
        "bot-full" => Some(vec![words(&[
            "cargo",
            "nextest",
            "run",
            "-p",
            "lgwks_bot",
            "--all-targets",
            "--locked",
            "--features",
            "full",
            "--profile",
            "ci",
            "-E",
            "not test(saturation_r32_tier)",
        ])]),
        "appcui" => {
            let storefront = [
                "-p",
                "lgwks_deps",
                "--no-default-features",
                "--features",
                "appcui",
            ];
            let with = |head: &[&str], tail: &[&str]| -> Vec<String> {
                words(head)
                    .into_iter()
                    .chain(words(&storefront))
                    .chain(words(tail))
                    .collect()
            };
            Some(vec![
                with(&["cargo", "test", "--locked"], &["--lib"]),
                with(&["cargo", "test", "--locked"], &["--doc"]),
                with(
                    &["cargo", "clippy", "--locked"],
                    &["--all-targets", "--", "-D", "warnings"],
                ),
            ])
        }
        _ => None,
    }
}

/// The name of the runner that owns the target volume: its `RUNNER_NAME`, or
/// `local` outside CI.
pub(crate) fn owner(runner_name: Option<&str>) -> String {
    match runner_name {
        Some(name) if !name.is_empty() => slug(name),
        Some(_) | None => "local".to_owned(),
    }
}

/// Every name the run gives a Docker object.
pub(crate) struct Names {
    /// The toolchain image, which is the base of the derived one.
    pub(crate) image: String,
    /// The derived image, which carries the linker and `sqlite3`.
    pub(crate) tools_image: String,
    /// The volume that holds the container's build output.
    pub(crate) target_volume: String,
    /// The volume that holds the toolchain's components.
    pub(crate) rustup_volume: String,
}

/// The names for a toolchain, a runner and a set of suites.
///
/// The target volume belongs to one runner: every runner on the machine talks
/// to the same engine, and a runner runs one job at a time, so its own volume
/// has one writer. The image is part of the volume's name too, because cargo's
/// fingerprints do not record the C library a build linked against: a binary a
/// trixie build left (glibc 2.39) failed to load in a bookworm run
/// (`GLIBC_2.39 not found`, run 38099333925). One image, one volume. The rustup
/// volume is named by the toolchain, so a pin change starts a fresh one rather
/// than mixing two toolchains.
pub(crate) fn names(toolchain: &str, owner: &str, suites: &[String]) -> Names {
    let image = format!("rust:{toolchain}-{DEBIAN}");
    let tag = slug(&image);
    Names {
        tools_image: format!("lwc-ci-{tag}-tools"),
        target_volume: format!("lwc-ci-linux-target-{owner}-{tag}-{}", suites.join("-")),
        rustup_volume: format!("lwc-ci-linux-rustup-{}-{owner}", slug(toolchain)),
        image,
    }
}

/// The Dockerfile of the derived image.
pub(crate) fn dockerfile(image: &str) -> String {
    format!(
        "FROM {image}\nRUN apt-get update -qq && apt-get install -y -qq {IMAGE_PACKAGES} && rm -rf /var/lib/apt/lists/*\n"
    )
}

/// `argument` as one shell word.
fn quoted(argument: &str) -> String {
    let plain = !argument.is_empty()
        && argument.chars().all(|ch| {
            ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '/' | ':' | '=' | '-')
        });
    if plain {
        argument.to_owned()
    } else {
        format!("'{}'", argument.replace('\'', "'\\''"))
    }
}

/// An argument vector as one shell line.
pub(crate) fn shell_line(command: &[String]) -> String {
    command
        .iter()
        .map(|argument| quoted(argument))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The script the container runs.
///
/// A job killed mid-unpack leaves a package directory with no `.cargo-ok`, and
/// cargo never repairs one, so the next unpack fails with `File exists` (run
/// 37673046041: tree-sitter-nix). The sweep removes the partials before
/// anything builds: a directory with its `.cargo-ok` is a complete unpack and
/// stays. The nextest the host runs is installed into the tools volume when
/// the volume holds another version.
pub(crate) fn container_script(nextest: &str, platform: &str, commands: &[Vec<String>]) -> String {
    let mut lines = vec![
        "set -euo pipefail".to_owned(),
        format!(
            "if [ -d {REGISTRY_SOURCES} ]; then find {REGISTRY_SOURCES} -mindepth 2 -maxdepth 2 -type d ! -exec test -e '{{}}/.cargo-ok' ';' -exec sh -c 'echo \"linux-container-tests: removing partial unpack $1\"; rm -rf \"$1\"' sh '{{}}' ';'; fi"
        ),
        format!(
            "if [ \"$(/opt/tools/cargo-nextest --version 2>/dev/null | awk 'NR==1 {{print $2}}')\" != '{nextest}' ]; then curl -fsSL 'https://get.nexte.st/{nextest}/{platform}' | tar -xz -C /opt/tools; fi"
        ),
        "export PATH=/opt/tools:$PATH".to_owned(),
        "rustc --version".to_owned(),
    ];
    lines.extend(commands.iter().map(|command| shell_line(command)));
    lines.push(String::new());
    lines.join("\n")
}

/// The cgroup limits a measuring run is held to, as Docker spells them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Cap {
    /// `--cpus`: the CPU time the container may use, in CPUs.
    pub(crate) cpus: String,
    /// `--memory`, and `--memory-swap` to the same value, so the cap has no swap.
    pub(crate) memory: String,
    /// The ceiling of `/tmp`: tmpfs pages count against the memory cgroup, so a
    /// measuring run keeps it small.
    pub(crate) tmpfs: String,
}

/// One `docker run`: the commands it runs and the cap it runs under.
#[derive(Debug)]
pub(crate) struct Run {
    /// The commands, each an argument vector.
    pub(crate) commands: Vec<Vec<String>>,
    /// The limits the container is held to, or `None` for an uncapped run.
    pub(crate) cap: Option<Cap>,
}

/// What a `docker run` of the leg is made of.
pub(crate) struct Container<'run> {
    /// The Docker object names.
    pub(crate) names: &'run Names,
    /// The checkout, mounted at `/src`.
    pub(crate) root: &'run str,
    /// The machine's cargo configuration, mounted read-only when it exists.
    pub(crate) cargo_config: Option<&'run str>,
    /// The variable that carries the container target's rustflags.
    pub(crate) flags_variable: &'run str,
    /// The rustflags themselves.
    pub(crate) flags: &'run str,
    /// The debuginfo level of the test profile.
    pub(crate) test_debug: &'run str,
    /// The limits of a measuring run; `None` leaves the container uncapped.
    pub(crate) cap: Option<&'run Cap>,
    /// The script to run.
    pub(crate) script: &'run str,
}

impl Container<'_> {
    /// The arguments of `docker run`.
    ///
    /// `--init` puts a reaping init at PID 1, as a Linux host has. Without it
    /// the shell is PID 1, never reaps the descendants a supervised process
    /// orphans, and a killed descendant stays a zombie that `kill(pid, 0)`
    /// still reports present.
    ///
    /// `/tmp` is a tmpfs, for the reason the macOS jobs keep theirs on a RAM
    /// disk (WORKFLOW.md §12): the durable-store tests `fsync` every record,
    /// and in the VM each one reaches the host's disk through the virtual block
    /// device. No test simulates power loss, so no assertion depends on where
    /// the bytes land. Docker's tmpfs default is `noexec`; the suites execute
    /// scripts they write there (the fake `gh`), so the mount says `exec`. The
    /// size is a ceiling, not a reservation.
    ///
    /// A measuring run is capped by cgroup (`--cpus`, and `--memory` with
    /// `--memory-swap` equal to it, so the container cannot swap): the build
    /// that precedes it is not, so the cap measures the run and not the compile.
    pub(crate) fn arguments(&self) -> Vec<String> {
        let tmpfs = match self.cap {
            Some(cap) => cap.tmpfs.as_str(),
            None => TMPFS,
        };
        let mut arguments = words(&["run", "--rm", "--init"]);
        arguments.extend([
            "--tmpfs".to_owned(),
            format!("/tmp:rw,exec,nosuid,size={tmpfs}"),
        ]);
        if let Some(cap) = self.cap {
            arguments.extend(words(&[
                "--cpus",
                cap.cpus.as_str(),
                "--memory",
                cap.memory.as_str(),
                "--memory-swap",
                cap.memory.as_str(),
            ]));
        }
        let mut volume = |mount: String| arguments.extend(["--volume".to_owned(), mount]);
        volume(format!("{}:/src", self.root));
        volume(format!("{}:{CONTAINER_TARGET}", self.names.target_volume));
        volume("lwc-ci-linux-registry:/usr/local/cargo/registry".to_owned());
        volume("lwc-ci-linux-tools:/opt/tools".to_owned());
        volume(format!("{}:/usr/local/rustup", self.names.rustup_volume));
        if let Some(config) = self.cargo_config {
            volume(format!("{config}:/usr/local/cargo/config.toml:ro"));
        }
        for setting in [
            format!("{}={}", self.flags_variable, self.flags),
            format!("CARGO_TARGET_DIR={CONTAINER_TARGET}"),
            "NEXTEST_TEST_THREADS".to_owned(),
            format!("CARGO_PROFILE_TEST_DEBUG={}", self.test_debug),
            "RUSTC_WORKSPACE_WRAPPER=".to_owned(),
        ] {
            arguments.extend(["--env".to_owned(), setting]);
        }
        arguments.extend(words(&["--workdir", "/src"]));
        arguments.push(self.names.tools_image.clone());
        arguments.extend(words(&["bash", "-c"]));
        arguments.push(self.script.to_owned());
        arguments
    }
}
