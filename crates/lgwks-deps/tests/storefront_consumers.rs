//! Compile isolated consumers through the selected storefront feature paths.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn workspace_target_dir() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest
        .parent()
        .ok_or("crate manifest has no parent")?
        .parent()
        .ok_or("crates directory has no parent")?;
    Ok(workspace.join("target"))
}

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

        let tree = Command::new(env!("CARGO"))
            .args(["tree", "--locked", "--manifest-path"])
            .arg(&manifest)
            .args(["--no-default-features", "-e", "features"])
            .env("CARGO_TARGET_DIR", workspace_target_dir()?)
            .output()?;
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
