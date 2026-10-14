//! The Linux leg's decisions, checked without a container: the commands it runs,
//! the names it gives Docker, and what it reads from the machine's configuration.

use std::error::Error;
use std::path::PathBuf;

use crate::execute;
use crate::plan::{
    Container, SUITES, container_script, container_triple, dockerfile, host_triple, names,
    nextest_platform, nextest_version, owner, rustflags_variable, shell_line, slug, suite_commands,
    target_rustflags, toolchain_channel,
};
use crate::toml_subset::{read_tables, text};

type TestResult = Result<(), Box<dyn Error>>;

/// The runners that own a target volume on the CI machine (WORKFLOW.md §12: thirteen
/// instances), plus the name outside CI.
const OWNERS: [&str; 4] = [
    "MacBook-Pro-lwc-8",
    "MacBook-Pro-lwc-9",
    "MacBook-Pro-lwc-12",
    "local",
];

/// A cargo configuration in the shape the CI machine's is: a `[build]` table the
/// target table overrides, and a `rustflags` array that wraps over lines and
/// carries comments, one of them with an apostrophe (`Apple's`), which a string
/// reader that did not skip comments would take for an unterminated literal.
const CARGO_CONFIG: &str = "[build]\nrustflags = [\"-C\", \"debuginfo=0\"]\n\n[target.aarch64-apple-darwin]\nrustflags = [\n    # Use Apple's fast modern linker explicitly\n    \"-C\", \"link-arg=-fuse-ld=lld\",\n    # Build directly for M-series native instructions\n    \"-C\", \"target-cpu=native\",\n]\n\n[target.x86_64-apple-darwin]\nrustflags = [\"-C\", \"target-cpu=skylake\"]\n";

/// The text of a file at the repository root.
fn repo_file(name: &str) -> Result<String, Box<dyn Error>> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(name);
    Ok(std::fs::read_to_string(path)?)
}

/// The `command` of the lane called `id` in the lane table.
fn lane_command(lanes: &str, id: &str) -> Option<String> {
    read_tables(lanes)
        .iter()
        .filter(|table| table.path == "lane")
        .find(|table| text(table, "id") == Some(id))
        .and_then(|table| text(table, "command"))
        .map(str::to_owned)
}

/// Words as owned strings.
fn suites(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_owned()).collect()
}

#[test]
fn the_pinned_channel_is_the_one_the_image_is_built_from() -> TestResult {
    // rust-toolchain.toml is the repository's pin; the image must be that toolchain.
    let manifest = repo_file("rust-toolchain.toml")?;
    let channel = toolchain_channel(&manifest).ok_or("no channel in rust-toolchain.toml")?;
    let layout = names(&channel, "local", &suites(&["workspace"]));
    assert_eq!(
        layout.image,
        format!("rust:{channel}-trixie"),
        "the base image"
    );
    assert_eq!(
        toolchain_channel("[toolchain]\ncomponents = [\"clippy\"]\n"),
        None,
        "a manifest without a channel names none"
    );
    Ok(())
}

#[test]
fn the_image_is_the_debian_whose_setpriv_applies_landlock() -> TestResult {
    // util-linux 2.40 added Landlock to setpriv; trixie ships 2.41.5 and bookworm 2.38.1
    // (observed with `setpriv --version` in each image, 2026-10-13). On bookworm the
    // Landlock simulations fail with `Unsupported` (run 38455031363).
    let layout = names("1.99.0", "local", &suites(&["bot-full"]));
    assert!(layout.image.ends_with("-trixie"), "{}", layout.image);
    assert!(
        !layout.target_volume.contains("bookworm"),
        "a trixie build never shares a volume with a bookworm one: {}",
        layout.target_volume
    );
    Ok(())
}

#[test]
fn a_runner_keeps_the_volume_names_it_already_has() -> TestResult {
    // The names below are the volumes `docker volume ls` lists on the CI machine for
    // lwc-9 and lwc-8, so a runner finds its warm build output on the first run.
    let bot = names(
        "1.99.0",
        &owner(Some("MacBook-Pro-lwc-9")),
        &suites(&["bot-full", "appcui"]),
    );
    assert_eq!(
        bot.target_volume,
        "lwc-ci-linux-target-MacBook-Pro-lwc-9-rust-1.99.0-trixie-bot-full-appcui",
        "the bot-full volume"
    );
    assert_eq!(
        bot.rustup_volume, "lwc-ci-linux-rustup-1.99.0-MacBook-Pro-lwc-9",
        "the rustup volume"
    );
    let work = names(
        "1.99.0",
        &owner(Some("MacBook-Pro-lwc-8")),
        &suites(&["workspace"]),
    );
    assert_eq!(
        work.target_volume, "lwc-ci-linux-target-MacBook-Pro-lwc-8-rust-1.99.0-trixie-workspace",
        "the workspace volume"
    );
    assert_eq!(owner(None), "local", "outside CI");
    assert_eq!(owner(Some("")), "local", "an empty runner name");
    assert_eq!(owner(Some("a b/c")), "a-b-c", "a name Docker would refuse");
    Ok(())
}

#[test]
fn no_two_runners_or_suite_sets_share_a_writable_volume() -> TestResult {
    // One writer per target volume: two runners on one volume relinked a test binary
    // while another executed it (`No such file or directory`, bot-full).
    let sets: Vec<Vec<String>> = {
        let mut found = Vec::new();
        for first in SUITES {
            found.push(suites(&[first]));
            for second in SUITES.iter().copied().filter(|other| *other != first) {
                found.push(suites(&[first, second]));
                for third in SUITES
                    .iter()
                    .copied()
                    .filter(|other| *other != first && *other != second)
                {
                    found.push(suites(&[first, second, third]));
                }
            }
        }
        found
    };
    assert_eq!(
        sets.len(),
        15,
        "three singles, six pairs, six orderings of all three"
    );
    let mut targets = Vec::new();
    for runner in OWNERS {
        for set in &sets {
            targets.push(names("1.99.0", &owner(Some(runner)), set).target_volume);
        }
    }
    let total = targets.len();
    targets.sort();
    targets.dedup();
    assert_eq!(
        targets.len(),
        total,
        "every runner and suite set has its own target volume"
    );
    let one = names("1.99.0", "MacBook-Pro-lwc-8", &suites(&["workspace"]));
    let other = names("1.99.0", "MacBook-Pro-lwc-9", &suites(&["workspace"]));
    assert_ne!(
        one.rustup_volume, other.rustup_volume,
        "two first-time rustup installs would race"
    );
    assert_eq!(
        one.tools_image, other.tools_image,
        "the derived image is shared by every runner"
    );
    Ok(())
}

#[test]
fn the_commands_are_the_ones_the_lane_table_names() -> TestResult {
    // scripts/gate-lanes.toml is the one definition of the gate (INV-GOV-1): the Linux
    // leg runs the `tests` and `tests-bot-full` lanes' commands, nothing shorter.
    let lanes = repo_file("scripts/gate-lanes.toml")?;
    for (suite, lane) in [("workspace", "tests"), ("bot-full", "tests-bot-full")] {
        let command = lane_command(&lanes, lane).ok_or("a lane is missing from the table")?;
        let ours: Vec<String> = suite_commands(suite, None)
            .ok_or("a suite is unknown")?
            .iter()
            .map(|argv| shell_line(argv))
            .collect();
        assert!(
            command.starts_with(&ours.join(" && ")),
            "{suite} runs {ours:?} but the {lane} lane is {command}"
        );
    }
    Ok(())
}

#[test]
fn a_partition_shards_only_the_workspace_suite() -> TestResult {
    let sharded = suite_commands("workspace", Some("count:1/2")).ok_or("no workspace suite")?;
    let line = shell_line(sharded.first().ok_or("no command")?);
    assert!(line.ends_with("--partition count:1/2"), "{line}");
    let whole = suite_commands("workspace", None).ok_or("no workspace suite")?;
    assert!(
        !shell_line(whole.first().ok_or("no command")?).contains("--partition"),
        "unset, it is the whole suite"
    );
    for suite in ["bot-full", "appcui"] {
        let commands = suite_commands(suite, Some("count:1/2")).ok_or("a suite is missing")?;
        assert!(
            commands
                .iter()
                .all(|argv| !argv.contains(&"--partition".to_owned())),
            "{suite} is not sharded"
        );
    }
    assert!(
        suite_commands("everything", None).is_none(),
        "an unknown suite has no commands"
    );
    Ok(())
}

#[test]
fn every_suite_the_default_runs_has_commands() -> TestResult {
    for suite in SUITES {
        let commands = suite_commands(suite, None).ok_or("a default suite has no commands")?;
        assert!(!commands.is_empty(), "{suite} runs something");
        assert!(
            commands
                .iter()
                .all(|argv| argv.contains(&"--locked".to_owned())),
            "{suite} passes --locked to every cargo call"
        );
    }
    let appcui = suite_commands("appcui", None).ok_or("no appcui suite")?;
    assert_eq!(appcui.len(), 3, "the lib tests, the doc tests and the lint");
    Ok(())
}

#[test]
fn the_rustflags_are_the_host_targets_own_table() -> TestResult {
    // Cargo: a `[target.<triple>]` rustflags replaces `[build] rustflags` for that triple.
    let flags = target_rustflags(CARGO_CONFIG, "aarch64-apple-darwin")?;
    assert_eq!(
        flags,
        ["-C", "link-arg=-fuse-ld=lld", "-C", "target-cpu=native"],
        "the wrapped array, comments skipped, in order"
    );
    assert_eq!(
        target_rustflags(CARGO_CONFIG, "x86_64-apple-darwin")?,
        ["-C", "target-cpu=skylake"],
        "another host's table"
    );
    assert!(
        target_rustflags(CARGO_CONFIG, "riscv64gc-unknown-linux-gnu")?.is_empty(),
        "a triple with no table has no flags, and [build] is not borrowed"
    );
    assert!(
        target_rustflags("", "aarch64-apple-darwin")?.is_empty(),
        "no configuration"
    );
    let linker_only = "[target.aarch64-apple-darwin]\nlinker = \"clang\"\n";
    assert!(
        target_rustflags(linker_only, "aarch64-apple-darwin")?.is_empty(),
        "a table with no rustflags has none"
    );
    Ok(())
}

#[test]
fn cargos_string_form_of_rustflags_is_read_and_any_other_shape_is_refused() -> TestResult {
    // Cargo reference, `target.<triple>.rustflags`: "type: string or array of strings".
    let string = "[target.aarch64-apple-darwin]\nrustflags = \"-C target-cpu=native\"\n";
    assert_eq!(
        target_rustflags(string, "aarch64-apple-darwin")?,
        ["-C", "target-cpu=native"],
        "a space-separated string"
    );
    let number = "[target.aarch64-apple-darwin]\nrustflags = 5\n";
    let refused = target_rustflags(number, "aarch64-apple-darwin");
    assert!(
        matches!(refused, Err(ref failure) if failure.to_string().contains("neither a list nor a string")),
        "a number is refused by name, not built into a container without the machine's flags"
    );
    let array_of_tables = "[[target.aarch64-apple-darwin]]\nrustflags = [\"-C\", \"x\"]\n";
    assert!(
        target_rustflags(array_of_tables, "aarch64-apple-darwin")?.is_empty(),
        "an array of tables is not the target table"
    );
    Ok(())
}

#[test]
fn the_machine_is_read_the_way_rustc_and_nextest_print_it() {
    let rustc = "rustc 1.99.0 (abcdef012 2026-09-01)\nbinary: rustc\nhost: aarch64-apple-darwin\nrelease: 1.99.0\n";
    assert_eq!(
        host_triple(rustc),
        Some("aarch64-apple-darwin"),
        "rustc -vV"
    );
    assert_eq!(host_triple("rustc 1.99.0\n"), None, "no host line");
    let nextest = "cargo-nextest 0.9.104 (e5d1f0c9a 2026-08-12)\nrelease: 0.9.104\n";
    assert_eq!(
        nextest_version(nextest),
        Some("0.9.104"),
        "cargo nextest --version"
    );
    assert_eq!(nextest_version(""), None, "no output");
    assert_eq!(
        nextest_platform("aarch64"),
        Some("linux-arm"),
        "an Apple silicon host"
    );
    assert_eq!(nextest_platform("x86_64"), Some("linux"), "an Intel host");
    assert_eq!(nextest_platform("riscv64"), None, "no nextest build");
    assert_eq!(
        container_triple("arm64"),
        "aarch64-unknown-linux-gnu",
        "uname's spelling"
    );
    assert_eq!(
        container_triple("x86_64"),
        "x86_64-unknown-linux-gnu",
        "an Intel host"
    );
    assert_eq!(
        rustflags_variable("aarch64-unknown-linux-gnu"),
        "CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS",
        "the variable cargo reads"
    );
    assert_eq!(
        slug("rust:1.99.0-trixie"),
        "rust-1.99.0-trixie",
        "an image as a tag"
    );
}

#[test]
fn an_argument_reaches_the_shell_as_one_word() {
    let command = suites(&["cargo", "nextest", "-E", "not test(it's)", "plain/path:1=2"]);
    assert_eq!(
        shell_line(&command),
        "cargo nextest -E 'not test(it'\\''s)' plain/path:1=2",
        "spaces, parentheses and a quote are quoted; a plain word is not"
    );
    assert_eq!(
        shell_line(&suites(&[""])),
        "''",
        "an empty argument is still a word"
    );
}

#[test]
fn the_script_sweeps_installs_nextest_and_then_runs_every_command() -> TestResult {
    let commands = suite_commands("appcui", None).ok_or("no appcui suite")?;
    let script = container_script("0.9.104", "linux-arm", &commands);
    let lines: Vec<&str> = script.lines().collect();
    assert_eq!(
        lines.first(),
        Some(&"set -euo pipefail"),
        "stops at the first failure"
    );
    assert!(
        script.contains("! -exec test -e '{}/.cargo-ok'"),
        "a partial unpack is found by its missing .cargo-ok"
    );
    assert!(
        script.contains("https://get.nexte.st/0.9.104/linux-arm"),
        "the host's nextest version and archive"
    );
    let version = lines
        .iter()
        .position(|line| *line == "rustc --version")
        .ok_or("no rustc --version")?;
    for command in &commands {
        let position = lines
            .iter()
            .position(|line| *line == shell_line(command))
            .ok_or("a command is missing from the script")?;
        assert!(
            position > version,
            "{command:?} runs after the toolchain is named"
        );
    }
    assert!(script.ends_with('\n'), "the last command is a whole line");
    Ok(())
}

#[test]
fn the_derived_image_carries_the_linker_and_sqlite() {
    let text = dockerfile("rust:1.99.0-trixie");
    assert!(text.starts_with("FROM rust:1.99.0-trixie\n"), "{text}");
    assert!(
        text.contains("apt-get install -y -qq lld sqlite3"),
        "{text}"
    );
    assert!(
        text.contains("rm -rf /var/lib/apt/lists/*"),
        "the layer holds no apt index: {text}"
    );
}

#[test]
fn the_run_is_an_ephemeral_init_container_on_the_runners_own_volumes() {
    let layout = names(
        "1.99.0",
        "MacBook-Pro-lwc-9",
        &suites(&["bot-full", "appcui"]),
    );
    let container = Container {
        names: &layout,
        root: "/srv/lwc-ci/r9/work",
        cargo_config: Some("/srv/cargo/config.toml"),
        flags_variable: "CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS",
        flags: "-C target-cpu=native",
        test_debug: "line-tables-only",
        script: "echo hello world",
    };
    let arguments = container.arguments();
    let joined = arguments.join(" ");
    assert_eq!(
        arguments.first().map(String::as_str),
        Some("run"),
        "docker run"
    );
    for expected in [
        "--rm --init",
        "--tmpfs /tmp:rw,exec,nosuid,size=3g",
        "--volume /srv/lwc-ci/r9/work:/src",
        "--volume lwc-ci-linux-target-MacBook-Pro-lwc-9-rust-1.99.0-trixie-bot-full-appcui:/target",
        "--volume lwc-ci-linux-registry:/usr/local/cargo/registry",
        "--volume lwc-ci-linux-tools:/opt/tools",
        "--volume lwc-ci-linux-rustup-1.99.0-MacBook-Pro-lwc-9:/usr/local/rustup",
        "--volume /srv/cargo/config.toml:/usr/local/cargo/config.toml:ro",
        "--env CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS=-C target-cpu=native",
        "--env CARGO_TARGET_DIR=/target",
        "--env NEXTEST_TEST_THREADS",
        "--env CARGO_PROFILE_TEST_DEBUG=line-tables-only",
        "--env RUSTC_WORKSPACE_WRAPPER= --workdir /src lwc-ci-rust-1.99.0-trixie-tools bash -c echo hello world",
    ] {
        assert!(
            joined.contains(expected),
            "{expected} is missing from {joined}"
        );
    }
    assert!(
        !joined.contains("--privileged"),
        "the container is unprivileged"
    );
    assert_eq!(
        arguments.last().map(String::as_str),
        Some("echo hello world"),
        "the script is one argument"
    );
    assert_eq!(
        arguments,
        container.arguments(),
        "the same inputs give the same arguments"
    );
    let bare = Container {
        cargo_config: None,
        ..container
    };
    assert!(
        !bare.arguments().join(" ").contains("config.toml"),
        "no configuration on the machine, none mounted"
    );
}

#[test]
fn an_unknown_suite_is_refused_before_anything_runs() {
    let refused = execute(&suites(&["workspace", "nonsense"]));
    assert_eq!(
        refused.err().map(|failure| failure.to_string()),
        Some("unknown suite nonsense (workspace, bot-full, appcui)".to_owned()),
        "the refusal names the suite and the ones that exist"
    );
}
