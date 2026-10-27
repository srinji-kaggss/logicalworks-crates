//! Compile isolated consumers through the selected storefront feature paths.
//!
//! One test per facade and per storefront family, each its own `cargo check` of
//! an external consumer: a family is a separate compile of a separate feature
//! set, so it is a separate test with its own nextest bound. One test that walked
//! all seventeen families in a loop needed all of them to fit one 300 s bound,
//! and on a runner whose dependency builds were partly cold it did not (run
//! 37519838915). They share the target directory, so `.config/nextest.toml` runs
//! this binary's tests one at a time rather than parking them on cargo's lock.

use std::path::Path;
use std::process::{Command, Output};

use lgwks_std::random;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[path = "support/target_dir.rs"]
mod target_dir;

use target_dir::workspace_target_dir;

fn cargo_check(manifest: &Path, args: &[&str]) -> Result<Output, Box<dyn std::error::Error>> {
    let output = Command::new(env!("CARGO"))
        .arg("check")
        .arg("--locked")
        .arg("--manifest-path")
        .arg(manifest)
        .args(args)
        .env("CARGO_TARGET_DIR", workspace_target_dir()?)
        .output()?;
    Ok(output)
}

/// Run the `lgwks-deps` binary itself: the doctor under test, not a fixture
/// consumer. The binary is the crate's default run, and the `sqlite` command
/// rides the `sqlite` feature, so both are named outright.
fn cargo_run_bin(manifest: &Path, args: &[&str]) -> Result<Output, Box<dyn std::error::Error>> {
    let mut full: Vec<&str> = vec!["--features", "sqlite", "--bin", "lgwks-deps", "--"];
    full.extend(args.iter().copied());
    cargo_run(manifest, &full)
}

/// Names scratch stores for the sqlite doctor test with the estate's entropy
/// source: the OS never reuses a nonce the way it reuses a process id, so two
/// runs never share a file without reaching for a clock.
fn doctor_scratch() -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    let nonce = lgwks_std::hex::encode(random::bytes::<8>()?);
    Ok(std::env::temp_dir().join(format!("lgwks-deps-sqlite-{nonce}")))
}

/// Run one storefront example to completion: a compile proves the facade
/// resolves, and only a run proves the journey it sells.
fn cargo_run(manifest: &Path, args: &[&str]) -> Result<Output, Box<dyn std::error::Error>> {
    let output = Command::new(env!("CARGO"))
        .arg("run")
        .arg("--locked")
        .arg("--manifest-path")
        .arg(manifest)
        .args(args)
        .env("CARGO_TARGET_DIR", workspace_target_dir()?)
        .output()?;
    Ok(output)
}

/// The consumer's feature graph with its default features off, which is the
/// graph a deselected storefront path must not reach.
fn cargo_tree_without_defaults(manifest: &Path) -> Result<Output, Box<dyn std::error::Error>> {
    let output = Command::new(env!("CARGO"))
        .args(["tree", "--locked", "--manifest-path"])
        .arg(manifest)
        .args(["--no-default-features", "-e", "features"])
        .env("CARGO_TARGET_DIR", workspace_target_dir()?)
        .output()?;
    Ok(output)
}

fn output_text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// The fixture directory every consumer manifest lives under.
fn fixtures() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// A Bevy facade consumer compiles with its facade selected, fails to resolve
/// `engine` with it deselected, and its default-off graph carries neither the
/// engine nor the `scan` parser stack.
fn bevy_facade_is_selected_only(consumer: &str, engine: &str) -> TestResult {
    let manifest = fixtures().join(consumer).join("Cargo.toml");
    let enabled = cargo_check(&manifest, &["--features", "selected"])?;
    assert!(
        enabled.status.success(),
        "{consumer} facade consumer failed:\n{}",
        output_text(&enabled)
    );

    let disabled = cargo_check(
        &manifest,
        &[
            "--no-default-features",
            "--features",
            "probe",
            "--example",
            "feature_off",
        ],
    )?;
    assert!(
        !disabled.status.success(),
        "{consumer} path compiled with its facade feature disabled"
    );
    let disabled_text = output_text(&disabled);
    assert!(
        (disabled_text.contains("unresolved import")
            || disabled_text.contains("could not find")
            || disabled_text.contains("cannot find"))
            && disabled_text.contains(engine),
        "{consumer} negative control failed for an unrelated reason:\n{disabled_text}"
    );

    let tree = cargo_tree_without_defaults(&manifest)?;
    let tree_text = output_text(&tree);
    assert!(tree.status.success(), "{tree_text}");
    assert!(
        !tree_text.contains(engine),
        "{consumer} disabled graph still contains {engine}:\n{tree_text}"
    );
    assert!(
        !tree_text.contains("lgwks_deps feature \"scan\""),
        "{consumer} default-features=false graph acquired scan parser dependencies:\n{tree_text}"
    );
    Ok(())
}

/// The storefront matrix consumer compiles `example` with only `feature` on.
fn storefront_family_compiles_alone(feature: &str, example: &str) -> TestResult {
    let manifest = fixtures().join("storefront-matrix/Cargo.toml");
    let output = cargo_check(
        &manifest,
        &[
            "--no-default-features",
            "--features",
            feature,
            "--example",
            example,
        ],
    )?;
    assert!(
        output.status.success(),
        "storefront-only {feature} consumer failed:\n{}",
        output_text(&output)
    );
    Ok(())
}

/// One `#[test]` per Bevy facade consumer.
macro_rules! bevy_facades {
    ($($name:ident => ($consumer:literal, $engine:literal),)+) => {$(
        #[test]
        fn $name() -> TestResult {
            bevy_facade_is_selected_only($consumer, $engine)
        }
    )+};
}

bevy_facades! {
    bevy_app_facade_is_selected_only => ("bevy-app", "bevy_app"),
    bevy_time_facade_is_selected_only => ("bevy-time", "bevy_time"),
    bevy_state_facade_is_selected_only => ("bevy-state", "bevy_state"),
}

/// One `#[test]` per storefront family, each with an optional `cfg` for a
/// family that only builds on one target.
macro_rules! storefront_families {
    ($($(#[$cfg:meta])* $name:ident => ($feature:literal, $example:literal),)+) => {$(
        #[test]
        $(#[$cfg])*
        fn $name() -> TestResult {
            storefront_family_compiles_alone($feature, $example)
        }
    )+};
}

storefront_families! {
    storefront_appcui_compiles_alone => ("appcui", "appcui"),
    storefront_bevy_ecs_compiles_alone => ("bevy-ecs", "bevy_ecs"),
    storefront_tokio_base_compiles_alone => ("tokio-base", "tokio_base"),
    storefront_tokio_time_compiles_alone => ("tokio-time", "tokio_time"),
    storefront_tokio_sync_compiles_alone => ("tokio-sync", "tokio_sync"),
    storefront_tokio_macros_compiles_alone => ("tokio-macros", "tokio_macros"),
    storefront_tokio_io_compiles_alone => ("tokio-io", "tokio_io"),
    storefront_tokio_net_compiles_alone => ("tokio-net", "tokio_net"),
    storefront_tokio_process_compiles_alone => ("tokio-process", "tokio_process"),
    storefront_tokio_fs_compiles_alone => ("tokio-fs", "tokio_fs"),
    storefront_tokio_signal_compiles_alone => ("tokio-signal", "tokio_signal"),
    storefront_tokio_full_compiles_alone => ("tokio-full", "tokio_full"),
    storefront_ml_candle_compiles_alone => ("ml-candle", "ml_candle"),
    storefront_ml_tokenizers_compiles_alone => ("ml-tokenizers", "ml_tokenizers"),
    storefront_sqlite_compiles_alone => ("sqlite", "sqlite"),
    storefront_gpui_compiles_alone => ("gpui", "gpui"),
    #[cfg(target_os = "macos")]
    storefront_ml_candle_metal_compiles_alone => ("ml-candle-metal", "ml_candle_metal"),
}

/// The sqlite journey runs, not just compiles: open with the estate defaults,
/// migrate once, reopen and read back, refuse a downgrade. The example exits
/// non-zero on the first refusal, so the status is the verdict.
#[test]
fn storefront_sqlite_runs_its_journey() -> TestResult {
    let manifest = fixtures().join("storefront-matrix/Cargo.toml");
    let output = cargo_run(&manifest, &["--features", "sqlite", "--example", "sqlite"])?;
    assert!(
        output.status.success(),
        "sqlite journey failed:\n{}",
        output_text(&output)
    );
    Ok(())
}

#[test]
fn the_storefront_matrix_default_graph_carries_no_optional_family() -> TestResult {
    let manifest = fixtures().join("storefront-matrix/Cargo.toml");
    let tree = cargo_tree_without_defaults(&manifest)?;
    let tree_text = output_text(&tree);
    assert!(tree.status.success(), "{tree_text}");
    assert!(
        !tree_text.contains("lgwks_deps feature \"scan\""),
        "default-features=false matrix consumer acquired the scan feature:\n{tree_text}"
    );
    for optional_package in [
        "appcui v",
        "bevy_ecs v",
        "gpui v",
        "candle-core v",
        "tokenizers v",
        "rusqlite v",
        "tokio v",
        "nix v",
    ] {
        assert!(
            !tree_text.contains(optional_package),
            "default-features=false consumer acquired {optional_package}:\n{tree_text}"
        );
    }
    Ok(())
}

/// The `sqlite` doctor reports a store and migrates it: open with the estate
/// defaults, report the version the file carries, run one statement batch to
/// version 1, and report the recorded version on a reopen. The scratch store
/// is named with the estate's entropy source, and its removal is best-effort.
#[test]
fn storefront_sqlite_doctor_reports_and_migrates() -> TestResult {
    let dir = doctor_scratch()?;
    std::fs::create_dir_all(&dir)?;
    let store = dir.join("store.sqlite");
    let store_text = store.display().to_string();
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");

    let first = cargo_run_bin(&manifest, &["sqlite", store_text.as_str()])?;
    assert!(
        first.status.success(),
        "sqlite report failed:\n{}",
        output_text(&first)
    );
    let first_text = output_text(&first);
    assert!(
        first_text.contains("user_version=0"),
        "sqlite report hid user_version=0:\n{first_text}"
    );
    assert!(
        first_text.contains("journal_mode=wal"),
        "sqlite report hid journal_mode=wal:\n{first_text}"
    );

    let migrated = cargo_run_bin(
        &manifest,
        &[
            "sqlite",
            store_text.as_str(),
            "1",
            "CREATE TABLE t(k TEXT PRIMARY KEY);",
        ],
    )?;
    assert!(
        migrated.status.success(),
        "sqlite migrate failed:\n{}",
        output_text(&migrated)
    );
    let migrated_text = output_text(&migrated);
    assert!(
        migrated_text.contains("user_version=1"),
        "sqlite migrate hid user_version=1:\n{migrated_text}"
    );

    let again = cargo_run_bin(&manifest, &["sqlite", store_text.as_str()])?;
    assert!(
        again.status.success(),
        "sqlite re-report failed:\n{}",
        output_text(&again)
    );
    let again_text = output_text(&again);
    assert!(
        again_text.contains("user_version=1"),
        "sqlite re-report hid user_version=1:\n{again_text}"
    );

    drop(std::fs::remove_dir_all(&dir));
    Ok(())
}

/// A process-only `lgwks_bot` consumer compiles with `default-features = false`
/// and carries none of the seven deselected crates (#687): the ECS substrate,
/// the wire and RON encodings, the tracing installer, or the TLS/HTTP stack.
#[test]
fn proc_only_consumer_carries_no_deselected_crate() -> TestResult {
    let manifest = fixtures().join("process-only/Cargo.toml");
    let checked = cargo_check(&manifest, &[])?;
    assert!(
        checked.status.success(),
        "process-only consumer failed:\n{}",
        output_text(&checked)
    );
    let tree = cargo_tree_without_defaults(&manifest)?;
    let tree_text = output_text(&tree);
    assert!(tree.status.success(), "{tree_text}");
    for absent in [
        "bevy_ecs v",
        "rkyv v",
        "ron v",
        "tracing-subscriber v",
        "ureq v",
        "rustls v",
        "ring v",
    ] {
        assert!(
            !tree_text.contains(absent),
            "process-only tree contains deselected {absent}:\n{tree_text}"
        );
    }
    Ok(())
}
