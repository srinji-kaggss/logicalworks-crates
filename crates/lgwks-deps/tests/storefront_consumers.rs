//! Compile isolated consumers through the selected storefront feature paths.

use std::path::Path;
use std::process::{Command, Output};

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

#[test]
fn minimal_external_consumers_use_selected_bevy_facades_only() -> TestResult {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    for (consumer, engine) in [
        ("bevy-app", "bevy_app"),
        ("bevy-time", "bevy_time"),
        ("bevy-state", "bevy_state"),
    ] {
        let manifest = fixtures.join(consumer).join("Cargo.toml");
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
            "{consumer} negative control failed for an unrelated reason:\n{}",
            output_text(&disabled)
        );

        let tree = cargo_tree_without_defaults(&manifest)?;
        assert!(tree.status.success(), "{}", output_text(&tree));
        assert!(
            !output_text(&tree).contains(engine),
            "{consumer} disabled graph still contains {engine}:\n{}",
            output_text(&tree)
        );
        assert!(
            !output_text(&tree).contains("lgwks_deps feature \"scan\""),
            "{consumer} default-features=false graph acquired scan parser dependencies:\n{}",
            output_text(&tree)
        );
    }
    Ok(())
}

#[test]
fn minimal_external_consumers_exercise_each_storefront_family() -> TestResult {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let manifest = fixtures.join("storefront-matrix/Cargo.toml");
    let mut selections = vec![
        ("appcui", "appcui"),
        ("bevy-ecs", "bevy_ecs"),
        ("tokio-base", "tokio_base"),
        ("tokio-time", "tokio_time"),
        ("tokio-sync", "tokio_sync"),
        ("tokio-macros", "tokio_macros"),
        ("tokio-io", "tokio_io"),
        ("tokio-net", "tokio_net"),
        ("tokio-process", "tokio_process"),
        ("tokio-fs", "tokio_fs"),
        ("tokio-signal", "tokio_signal"),
        ("tokio-full", "tokio_full"),
        ("ml-candle", "ml_candle"),
        ("ml-tokenizers", "ml_tokenizers"),
        ("gpui", "gpui"),
        ("ml-candle-metal", "ml_candle_metal"),
        ("process-group-probe", "process_group_probe"),
    ];
    if !cfg!(target_os = "macos") {
        selections.retain(|&(feature, _)| feature != "ml-candle-metal");
    }
    if !cfg!(unix) {
        selections.retain(|&(feature, _)| feature != "process-group-probe");
    }

    for (feature, example) in selections {
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
    }

    let tree = cargo_tree_without_defaults(&manifest)?;
    assert!(tree.status.success(), "{}", output_text(&tree));
    assert!(
        !output_text(&tree).contains("lgwks_deps feature \"scan\""),
        "default-features=false matrix consumer acquired the scan feature:\n{}",
        output_text(&tree)
    );
    for optional_package in [
        "appcui v",
        "bevy_ecs v",
        "gpui v",
        "candle-core v",
        "tokenizers v",
        "tokio v",
        "nix v",
    ] {
        assert!(
            !output_text(&tree).contains(optional_package),
            "default-features=false consumer acquired {optional_package}:\n{}",
            output_text(&tree)
        );
    }
    Ok(())
}
