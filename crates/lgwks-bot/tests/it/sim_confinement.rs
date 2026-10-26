//! Simulation family: confinement is declared as data, validated before the
//! fork, and refused fail-closed where the platform has no sandbox (#337).
//!
//! Every scenario draws a [`ProcessSpec`] shape — program, arguments,
//! environment deltas, deadline and confinement — and checks the spec's own
//! accessors against an independent model of what was set. The spec is data,
//! so the model is the draw list itself: a setter the accessors do not report
//! is a description the supervisor cannot be audited against.
//!
//! The sandbox-profile validation beside it is pure: the seed draws profile
//! sources from an alphabet with hostile members, and the model decides
//! validity from the documented rule. A profile the model refuses must refuse
//! the spawn too, on every target, so a child never runs unconfined because
//! its profile was damage.
//!
//! | Family | Property it pins |
//! |---|---|
//! | `shape` | every setter round-trips through its accessor, and validation agrees with the model |
//! | `bands` | the seed space partitions with no gap and no overlap |

#![cfg(all(
    unix,
    feature = "rt",
    feature = "time",
    feature = "sync",
    feature = "process"
))]

use crate::sim;

use std::error::Error;
use std::num::NonZeroUsize;
use std::time::Duration;

use lgwks_bot::Runtime;
use lgwks_bot::rt::process::{Confinement, DenyBeneath, ProcessSpec, SandboxProfile};
use lgwks_bot::rt::supervise::Supervisor;

/// What a scenario reports when a precondition did not hold.
type TestResult = Result<(), Box<dyn Error>>;

/// The seed space this family sweeps, and how many bands it is cut into.
const SEEDS: u64 = 160;
const PARTS: u64 = 4;

/// Programs a drawn spec names; absolute paths, because a cleared child has
/// no supervisor `PATH` to search.
const PROGRAMS: [&str; 3] = ["/bin/echo", "/usr/bin/true", "/usr/bin/env"];

/// Words drawn arguments and values are built from.
const WORDS: [&str; 6] = ["alpha", "beta", "gamma", "delta", "omega", "zed"];

/// Bytes a profile-source draw may hold, hostile members included. The model
/// below accepts bytes at or above `0x80` as UTF-8 text a denied non-ASCII
/// path is spelled in, so the alphabet carries only the ASCII range the
/// validator can refuse.
const PROFILE_BYTES: &[u8] = b"()version 1allowdefault-/_. \t\n\x01\x7f+(subpath)";

/// The model's validity, restated from the documented rule: empty and
/// oversized sources are refused, and only ASCII control bytes other than tab
/// and newline are refused — bytes at or above `0x80` are UTF-8 text a denied
/// non-ASCII path is spelled in, not damage.
fn model_valid(source: &str) -> bool {
    !source.is_empty()
        && source.len() <= SandboxProfile::MAX_SOURCE_BYTES
        && source
            .bytes()
            .all(|byte| byte >= 0x80 || byte.is_ascii_graphic() || byte.is_ascii_whitespace())
}

/// One drawn index below `len`.
fn draw_index(sim: &mut sim::Sim, len: usize) -> Result<usize, Box<dyn Error>> {
    let bound = u32::try_from(len)
        .map_err(|error| format!("a draw alphabet of {len} entries fits u32: {error}"))?;
    usize::try_from(sim.rng().below(bound))
        .map_err(|error| -> Box<dyn Error> { format!("a u32 draw fits usize: {error}").into() })
}

/// One drawn profile source.
fn draw_source(sim: &mut sim::Sim) -> Result<String, Box<dyn Error>> {
    let length = if sim.rng().below(16) == 0 {
        // Rarely overlong: the bound is the property under test.
        SandboxProfile::MAX_SOURCE_BYTES
            .checked_add(
                usize::try_from(sim.rng().below(16))
                    .map_err(|error| format!("a u32 draw fits usize: {error}"))?,
            )
            .ok_or("the overlong draw fits usize")?
    } else {
        draw_index(sim, 96)?
    };
    let mut source = String::new();
    for _ in 0..length {
        source.push(char::from(
            PROFILE_BYTES[draw_index(sim, PROFILE_BYTES.len())?],
        ));
    }
    Ok(source)
}

/// One band of the shape family, swept twice for the replay receipt.
fn shape_band(index: usize) -> TestResult {
    sim::assert_replays(sim::bands(SEEDS, PARTS).swap_remove(index), shape_case)
}

/// One seed: draw a spec shape, check the accessors against the draw.
fn shape_case(sim: &mut sim::Sim) -> TestResult {
    let program = PROGRAMS[draw_index(sim, PROGRAMS.len())?];
    let mut spec = ProcessSpec::new(program);
    sim.trace.record(&format!("program {program}"));
    assert_eq!(
        spec.program().to_string_lossy(),
        program,
        "the program accessor reports what new was given"
    );

    let mut drawn_args = 0_usize;
    for _ in 0..draw_index(sim, 4)? {
        let word = WORDS[draw_index(sim, WORDS.len())?];
        spec.arg(word);
        drawn_args = drawn_args.saturating_add(1);
        sim.trace.record(&format!("arg {word}"));
    }
    assert_eq!(
        spec.args().len(),
        drawn_args,
        "the argument accessor reports every arg call"
    );

    for _ in 0..draw_index(sim, 4)? {
        let key = WORDS[draw_index(sim, WORDS.len())?];
        match sim.rng().below(3) {
            0 => {
                spec.env(key, "drawn");
                sim.trace.record(&format!("env {key}"));
            }
            1 => {
                spec.env_remove(key);
                sim.trace.record(&format!("remove {key}"));
            }
            _ => {
                spec.env_clear();
                sim.trace.record("clear");
            }
        }
    }

    let confined = sim.rng().below(2) == 0;
    let source = draw_source(sim)?;
    if confined {
        spec.confinement(Confinement::SandboxProfile(SandboxProfile::new(
            source.clone(),
        )));
        sim.trace.record("confined");
    } else {
        sim.trace.record("unconfined");
    }
    match *spec.confinement_policy() {
        Confinement::None => {
            assert!(
                !confined,
                "a spec that never named a profile must read as unconfined"
            );
        }
        Confinement::SandboxProfile(ref profile) => {
            assert!(
                confined,
                "a spec that reads confined must have been told so"
            );
            assert_eq!(
                profile.source(),
                source.as_str(),
                "the profile accessor reports the source it was given"
            );
            assert_eq!(
                profile.validate().is_ok(),
                model_valid(&source),
                "validation must agree with the model on {source:?}"
            );
        }
        _ => {}
    }

    if sim.rng().below(2) == 0 {
        spec.deadline(Duration::from_secs(30));
        assert_eq!(
            spec.deadline_duration(),
            Some(Duration::from_secs(30)),
            "the deadline accessor reports what deadline was given"
        );
        sim.trace.record("deadline");
    } else {
        assert_eq!(
            spec.deadline_duration(),
            None,
            "a spec with no deadline must read as none"
        );
    }

    let again = spec.clone();
    assert_eq!(spec, again, "a spec is data: cloning it must compare equal");
    sim.record(if confined { "confined" } else { "plain" });
    Ok(())
}

macro_rules! shape_family {
    ($($name:ident => $index:expr);+ $(;)?) => {
        $(
            /// A seeded sweep of spec shapes against the draw model.
            #[test]
            fn $name() -> TestResult {
                shape_band($index)
            }
        )+
    };
}

shape_family!(
    shape_band_00 => 0; shape_band_01 => 1; shape_band_02 => 2; shape_band_03 => 3;
);

/// The bands partition the seed space: no gap, no overlap.
#[test]
fn the_confinement_bands_cover_every_seed_exactly_once() -> TestResult {
    let mut covered = Vec::new();
    for band in sim::bands(SEEDS, PARTS) {
        covered.extend(band.seeds());
    }
    covered.sort_unstable();
    covered.dedup();
    assert_eq!(
        u64::try_from(covered.len()).map_err(|_| "the seed count fits u64")?,
        SEEDS,
        "every seed is swept exactly once"
    );
    Ok(())
}

/// A cleared child inherits only what the owner names: the whole environment
/// it receives is exactly the two assignments after the clear.
#[test]
fn a_cleared_child_inherits_only_what_the_owner_names() -> TestResult {
    use std::collections::BTreeMap;

    let mut spec = ProcessSpec::new("/usr/bin/env");
    spec.capture_stdout(NonZeroUsize::new(1 << 20).ok_or("the capture ceiling is not zero")?);
    spec.env_clear()
        .env("LGWKS_ALLOW_A", "1")
        .env("LGWKS_ALLOW_B", "2");
    let runtime = Runtime::new()?;
    let mut supervisor = Supervisor::new(1);
    let run = runtime.block_on(supervisor.run_process(&spec))?;
    assert_eq!(run.exit_code(), Some(0), "/usr/bin/env exits zero");
    let text = std::str::from_utf8(run.stdout().bytes())?;
    let seen: BTreeMap<&str, &str> = text
        .lines()
        .map(|line| {
            line.split_once('=')
                .ok_or_else(|| format!("a printed variable has no `=`: {line:?}"))
        })
        .collect::<Result<_, _>>()?;
    assert_eq!(
        seen.len(),
        2,
        "a cleared child carries exactly the two named variables, not the supervisor's environment: {seen:?}"
    );
    assert_eq!(
        seen.get("LGWKS_ALLOW_A"),
        Some(&"1"),
        "the first named variable arrives"
    );
    assert_eq!(
        seen.get("LGWKS_ALLOW_B"),
        Some(&"2"),
        "the second named variable arrives"
    );
    assert!(
        !seen.contains_key("PATH"),
        "even PATH stays behind the clear unless the owner names it"
    );
    Ok(())
}

/// An invalid profile starts nothing: the spawn is refused and the marker the
/// child would have touched is absent.
#[test]
fn an_invalid_profile_starts_nothing() -> TestResult {
    use crate::scratch::Scratch;

    let dir = Scratch::new("confine-refused")?;
    let marker = dir.path().join("would-run");
    let mut spec = ProcessSpec::new("/bin/sh");
    spec.arg("-c").arg(format!("touch {}", marker.display()));
    // Empty is the smallest invalid profile; validation refuses it before the
    // fork on every target.
    spec.confinement(Confinement::SandboxProfile(SandboxProfile::new(
        String::new(),
    )));
    let runtime = Runtime::new()?;
    let mut supervisor = Supervisor::new(1);
    let refused = runtime.block_on(supervisor.run_process(&spec));
    assert!(
        refused.is_err(),
        "an invalid profile must refuse the spawn, not run the child"
    );
    assert!(
        !marker.exists(),
        "the refused child never ran, so its marker is absent"
    );
    Ok(())
}

/// A profile the platform has no sandbox for starts nothing either: the spawn
/// is refused as unsupported and the marker is absent. Fail-closed in the
/// other direction from the invalid profile above.
#[test]
#[cfg(not(target_os = "macos"))]
fn a_profile_without_a_platform_sandbox_starts_nothing() -> TestResult {
    use crate::scratch::Scratch;

    let dir = Scratch::new("confine-unsupported")?;
    let marker = dir.path().join("would-run");
    let mut spec = ProcessSpec::new("/bin/sh");
    spec.arg("-c").arg(format!("touch {}", marker.display()));
    spec.confinement(Confinement::SandboxProfile(SandboxProfile::new(
        "(version 1)(allow default)".to_owned(),
    )));
    let runtime = Runtime::new()?;
    let mut supervisor = Supervisor::new(1);
    let refused = runtime.block_on(supervisor.run_process(&spec));
    assert!(
        refused.is_err(),
        "a profile with no platform sandbox must refuse the spawn, not run the child unconfined"
    );
    assert!(
        !marker.exists(),
        "the refused child never ran, so its marker is absent"
    );
    Ok(())
}

/// A sandboxed child is denied its denied subtree, and allowed everything else.
///
/// The profile allows by default and denies writes under one scratch
/// directory. The confined write fails and leaves no file; the confined
/// `true` exits zero, proving the profile confines rather than breaks; and
/// the cleanup still settles, proving the wrapper stays inside the group the
/// supervisor owns.
#[test]
#[cfg(target_os = "macos")]
fn a_sandboxed_child_is_denied_its_denied_subtree() -> TestResult {
    use crate::scratch::Scratch;

    let dir = Scratch::new("sandbox")?;
    let denied = dir.path().join("denied");
    std::fs::create_dir_all(&denied)?;
    // Canonicalized: the sandbox matches the kernel's resolved path, and the
    // scratch directory arrives through a symlinked `$TMPDIR`, so the
    // unresolved spelling would name a subpath nothing writes through.
    let denied = std::fs::canonicalize(&denied)?;
    let profile = SandboxProfile::new(format!(
        "(version 1)(allow default)(deny file-write* (subpath \"{}\"))",
        denied.display()
    ));
    profile.validate()?;

    let runtime = Runtime::new()?;

    // The confined write fails: the kernel, not the supervisor, refused it.
    let mut denied_spec = ProcessSpec::new("/bin/sh");
    denied_spec
        .arg("-c")
        .arg(format!("echo hi > {}/probe", denied.display()));
    denied_spec.confinement(Confinement::SandboxProfile(profile.clone()));
    let mut supervisor = Supervisor::new(1);
    let denied_run = runtime.block_on(supervisor.run_process(&denied_spec))?;
    assert_ne!(
        denied_run.exit_code(),
        Some(0),
        "a write the profile denies must fail inside the sandbox"
    );
    assert!(
        !denied.join("probe").exists(),
        "the denied write left no file behind it"
    );

    // A confined sleeper stopped by its deadline proves the wrapper stays
    // inside the supervised group: the kill captures the tree, stops the
    // group, and settles confirmed with a complete containment report.
    let mut sleep_spec = ProcessSpec::new("/bin/sleep");
    sleep_spec.arg("30").deadline(Duration::from_secs(2));
    sleep_spec.confinement(Confinement::SandboxProfile(profile.clone()));
    let mut supervisor = Supervisor::new(1);
    let sleep_run = runtime.block_on(supervisor.run_process(&sleep_spec))?;
    assert!(
        sleep_run.deadline_fired(),
        "the confined sleeper must be stopped by its deadline, not exit on its own"
    );
    assert_eq!(
        sleep_run.cleanup(),
        &lgwks_bot::rt::supervise::CleanupReceipt::CleanupConfirmed,
        "the wrapper stays inside the supervised group, so the deadline kill settles"
    );
    assert!(
        sleep_run.containment().is_complete(),
        "the deadline kill names the tree it read and stops all of it: {:?}",
        sleep_run.containment()
    );

    // The confined `true` exits zero: the profile denies the subtree, not the
    // child's ordinary life.
    let mut true_spec = ProcessSpec::new("/usr/bin/true");
    true_spec.confinement(Confinement::SandboxProfile(profile));
    let mut supervisor = Supervisor::new(1);
    let true_run = runtime.block_on(supervisor.run_process(&true_spec))?;
    assert_eq!(
        true_run.exit_code(),
        Some(0),
        "a confined child with nothing denied about its work still exits zero"
    );
    Ok(())
}

/// Scope names this test probes with, disambiguating repeated runs.
///
/// A counter rather than the process id: pids are reused by the OS, so a pid
/// cannot distinguish two runs, while a counter in this binary can.
#[cfg(target_os = "linux")]
static TEST_SCOPE_IDS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// A cgroup scope kills a `setsid` escapee where the mount allows a scope.
///
/// The supervisor scopes every tree it starts, so this first probes whether
/// this host's mount admits one at all: without delegation the supervisor runs
/// its capture rounds instead, and that fallback is what
/// `tests/it/process_escape.rs` already proves. Where the mount admits a
/// scope, the escapee must die by the kernel owner, and the receipt must name
/// it.
#[test]
#[cfg(target_os = "linux")]
fn a_cgroup_scope_kills_a_setsid_escapee_where_the_mount_allows() -> TestResult {
    use std::io::ErrorKind;
    use std::time::Duration;

    use lgwks_bot::rt::supervise::{CleanupReceipt, ContainmentMechanism};
    use lgwks_std::process::CgroupScope;

    use crate::process_probe::{
        escape_command, escape_unavailable_reason, wait_for_pid, wait_for_pid_gone,
    };

    const BUDGET: Duration = Duration::from_secs(10);

    // The delegation probe: a scope of our own. Unsupported is the honest
    // fallback, and anything else is a failure of the probe, not of the tree.
    let probe = TEST_SCOPE_IDS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    match CgroupScope::create(&format!("lgwks-test-{probe}")) {
        Ok(scope) => drop(scope),
        Err(error) if error.kind() == ErrorKind::Unsupported => return Ok(()),
        Err(error) => return Err(error.into()),
    }

    let escape = escape_command().ok_or_else(escape_unavailable_reason)?;
    let dir = crate::scratch::Scratch::new("cgroup-escape")?;
    let escape_file = dir.path().join("escaped.pid");
    let leader_file = dir.path().join("leader.pid");
    let escape_script = format!(
        "echo $$ > {}; {}",
        leader_file.display(),
        escape.script(&escape_file)
    );

    let runtime = Runtime::new()?;
    let (cleanup, mechanism, escaped) = runtime.block_on(async {
        let mut supervisor = Supervisor::default();
        let mut spec = ProcessSpec::new("sh");
        spec.arg("-c").arg(&escape_script);
        supervisor.spawn_process(&spec).await?;
        let escaped = wait_for_pid(&escape_file, BUDGET)
            .ok_or_else(|| std::io::Error::other("the escaping child never recorded its pid"))?;
        let report = supervisor.shutdown().await;
        let outcome = report
            .outcomes()
            .first()
            .ok_or_else(|| std::io::Error::other("the shutdown reported no outcome"))?;
        let cleanup = outcome.cleanup().cloned().ok_or_else(|| {
            std::io::Error::other("a supervised process reported no cleanup receipt")
        })?;
        let mechanism = outcome
            .containment()
            .ok_or_else(|| std::io::Error::other("a supervised process reported no containment"))?
            .mechanism();
        Ok::<_, std::io::Error>((cleanup, mechanism, escaped))
    })?;

    assert!(
        wait_for_pid_gone(escaped, BUDGET).is_some(),
        "the setsid escapee must be gone after a scoped cleanup"
    );
    assert_eq!(
        cleanup,
        CleanupReceipt::CleanupConfirmed,
        "a scoped cleanup of a live-led tree settles confirmed"
    );
    assert_eq!(
        mechanism,
        ContainmentMechanism::CgroupKill,
        "where the mount admits a scope, the receipt must name the kernel owner that ran"
    );
    Ok(())
}

/// A profile source carrying UTF-8 validates, and only ASCII control bytes
/// other than tab and newline are refused (#690).
#[test]
fn sandbox_profile_accepts_utf8_and_refuses_control_bytes() -> TestResult {
    let ascii = SandboxProfile::try_new("(version 1)(allow default)")
        .map_err(|error| format!("a plain ASCII profile validates: {error}"))?;
    assert_eq!(
        ascii.source(),
        "(version 1)(allow default)",
        "try_new carries the validated source"
    );
    let utf8 = SandboxProfile::try_new(
        "(version 1)(allow default)(deny file-read* (subpath \"/srv/denied-josé\"))",
    )
    .map_err(|error| format!("a profile naming a non-ASCII path validates: {error}"))?;
    utf8.validate()
        .map_err(|error| format!("validation agrees with try_new on UTF-8: {error}"))?;
    for refused in ["a\x01b", "a\x7fb", ""] {
        assert!(
            SandboxProfile::try_new(refused).is_err(),
            "a control byte or an empty source refuses eagerly: {refused:?}"
        );
    }
    assert!(
        SandboxProfile::new("a\x01b").validate().is_err(),
        "validation refuses the control byte the constructor carried"
    );
    Ok(())
}

/// The builder canonicalizes the denied directory and quotes it for SBPL, so
/// a symlinked `$TMPDIR` and a `"` in the path name the resolved directory
/// (#690).
#[test]
fn the_builder_canonicalizes_quotes_and_denies() -> TestResult {
    use crate::scratch::Scratch;

    let dir = Scratch::new("builder-quote\"dir")?;
    let resolved = std::fs::canonicalize(dir.path())?;
    let mut profile = SandboxProfile::allow_default();
    profile
        .deny_read_beneath(dir.path())
        .map_err(|error| format!("the builder denies a real directory: {error}"))?;
    profile
        .validate()
        .map_err(|error| format!("the built profile validates: {error}"))?;
    let source = profile.source();
    assert!(
        source.starts_with("(version 1)(allow default)"),
        "the builder extends the default profile: {source:?}"
    );
    assert!(
        source.contains("(deny file-read*"),
        "the builder appends a read denial: {source:?}"
    );
    let quoted = resolved.to_string_lossy().replace('"', "\\\"");
    assert!(
        source.contains(quoted.as_str()),
        "the denial names the resolved directory, quoted for SBPL: {source:?}"
    );
    Ok(())
}

/// A profile denying a non-ASCII directory confines a `cat` of a file in it
/// on macOS (#690).
#[test]
#[cfg(target_os = "macos")]
fn a_deny_read_profile_confines_a_non_ascii_directory() -> TestResult {
    use crate::scratch::Scratch;

    let dir = Scratch::new("confine-josé")?;
    let denied = dir.path().join("denied");
    std::fs::create_dir_all(&denied)?;
    std::fs::write(denied.join("secret.txt"), b"secret")?;
    let mut profile = SandboxProfile::allow_default();
    profile
        .deny_read_beneath(&denied)
        .map_err(|error| format!("a non-ASCII denial builds: {error}"))?;
    profile
        .validate()
        .map_err(|error| format!("the non-ASCII profile validates: {error}"))?;

    let runtime = Runtime::new()?;
    let mut denied_spec = ProcessSpec::new("/bin/cat");
    denied_spec.arg(denied.join("secret.txt"));
    denied_spec.confinement(Confinement::SandboxProfile(profile.clone()));
    let mut supervisor = Supervisor::new(1);
    let denied_run = runtime.block_on(supervisor.run_process(&denied_spec))?;
    assert_ne!(
        denied_run.exit_code(),
        Some(0),
        "a read the profile denies fails inside the sandbox"
    );
    Ok(())
}

/// Denial validation agrees with its rule: non-empty, bounded, absolute, and
/// NUL-free (#689).
#[test]
fn deny_beneath_validation_agrees_with_its_rule() -> TestResult {
    let empty = DenyBeneath::new();
    assert!(empty.validate().is_err(), "an empty denial list refuses");
    let mut relative = DenyBeneath::new();
    relative.deny("relative/dir");
    assert!(relative.validate().is_err(), "a relative denial refuses");
    let mut many = DenyBeneath::new();
    for _ in 0..DenyBeneath::MAX_PATHS + 1 {
        many.deny("/srv/denied");
    }
    assert!(many.validate().is_err(), "a denial past the bound refuses");
    let mut one = DenyBeneath::new();
    one.deny("/srv/denied");
    one.validate()
        .map_err(|error| format!("one absolute denial validates: {error}"))?;
    assert_eq!(
        one.paths().count(),
        1,
        "the accessor reports the denial in order"
    );
    assert_eq!(
        Confinement::deny_beneath(["/srv/denied"]),
        Confinement::DenyBeneath(one),
        "the constructor builds the denial it names"
    );
    Ok(())
}

/// A lane denied its subtree cannot `cat` a file under it, on macOS through
/// the sandbox profile and on Linux through the mount-namespace overmount,
/// while the rest of the filesystem stays readable (#689).
#[test]
#[cfg(unix)]
fn a_deny_beneath_lane_cannot_read_its_denied_subtree() -> TestResult {
    use crate::scratch::Scratch;

    let dir = Scratch::new("deny-beneath")?;
    let denied = dir.path().join("denied");
    std::fs::create_dir_all(&denied)?;
    std::fs::write(denied.join("secret.txt"), b"secret")?;
    let public = dir.path().join("public.txt");
    std::fs::write(&public, b"public")?;

    let runtime = Runtime::new()?;
    let mut supervisor = Supervisor::new(1);

    // The denied read fails inside the confinement. A Linux host that refuses
    // every mount namespace (a container with neither `CAP_SYS_ADMIN` nor user
    // namespaces) refuses the spawn as unsupported before the fork instead:
    // fail-closed, so nothing ran, and that is the whole assertion there.
    let mut denied_spec = ProcessSpec::new("/bin/cat");
    denied_spec.arg(denied.join("secret.txt"));
    denied_spec.confinement(Confinement::deny_beneath([&denied]));
    let denied_run = match runtime.block_on(supervisor.run_process(&denied_spec)) {
        Ok(run) => run,
        Err(lgwks_bot::rt::process::ProcessRunError::NotStarted { source })
            if cfg!(target_os = "linux") && source.kind() == std::io::ErrorKind::Unsupported =>
        {
            return Ok(());
        }
        Err(other) => return Err(other.into()),
    };
    assert_ne!(
        denied_run.exit_code(),
        Some(0),
        "a read beneath the denied subtree fails inside the confinement"
    );

    // The public read succeeds: the denial takes one subtree, not the tree.
    let mut public_spec = ProcessSpec::new("/bin/cat");
    public_spec.arg(&public);
    public_spec.confinement(Confinement::deny_beneath([&denied]));
    let public_run = runtime.block_on(supervisor.run_process(&public_spec))?;
    assert_eq!(
        public_run.exit_code(),
        Some(0),
        "a read outside the denied subtree still succeeds"
    );
    Ok(())
}
