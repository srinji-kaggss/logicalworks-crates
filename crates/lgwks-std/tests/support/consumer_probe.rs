//! Shared harness for building a throwaway downstream consumer crate.
//!
//! Both `codec_contract.rs` and `wire_feature_unification.rs` need to compile
//! and run a minimal external consumer against this crate, so the setup lives
//! here once instead of being copied into each test target. It carries its own
//! tests rather than an `allow`: every probe target reads a different subset of
//! it, and a fixture that is only correct where its unread half is silenced is a
//! fixture nobody has read.

use std::path::Path;

/// Write `main_rs` into a fresh crate whose manifest is `manifest`, resolve its
/// lockfile offline, run it, and return its stdout.
///
/// # Errors
/// Returns the refusal when lock generation or the run fails, with the child's
/// stderr attached.
pub fn build_and_run(manifest: &str, main_rs: &str) -> Result<String, Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    std::fs::create_dir(directory.path().join("src"))?;
    let manifest_path = directory.path().join("Cargo.toml");
    std::fs::write(&manifest_path, manifest)?;
    std::fs::write(directory.path().join("src/main.rs"), main_rs)?;

    let cargo = |args: &[&str]| {
        std::process::Command::new(env!("CARGO"))
            .args(args)
            .arg("--manifest-path")
            .arg(&manifest_path)
            .env(
                "CARGO_TARGET_DIR",
                Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target"),
            )
            .output()
    };
    let lock = cargo(&["generate-lockfile", "--offline"])?;
    if !lock.status.success() {
        let refusal = Err(format!(
            "probe lock generation failed: {}",
            String::from_utf8_lossy(&lock.stderr)
        )
        .into());
        #[cfg(feature = "trace")]
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "build_and_run: returning an error to the caller");
        return refusal;
    }
    let run = cargo(&["run", "--locked", "--offline", "--quiet"])?;
    let stdout = String::from_utf8_lossy(&run.stdout).into_owned();
    if !run.status.success() {
        let refusal = Err(format!(
            "probe failed: {stdout}{}",
            String::from_utf8_lossy(&run.stderr)
        )
        .into());
        #[cfg(feature = "trace")]
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "build_and_run: returning an error to the caller");
        return refusal;
    }
    Ok(stdout)
}

/// Render a probe manifest named `package_name` that depends on this crate with
/// default features off and exactly `features` on.
pub fn manifest(package_name: &str, features: &[&str]) -> String {
    let features = if features.is_empty() {
        String::new()
    } else {
        let quoted: Vec<String> = features
            .iter()
            .map(|feature| format!("{feature:?}"))
            .collect();
        format!(", features = [{}]", quoted.join(", "))
    };
    format!(
        "[package]\nname = {package_name:?}\nversion = \"0.1.0\"\nedition = \"2024\"\n\n\
         [dependencies]\nlgwks_std = {{ path = {:?}, default-features = false{features} }}\n",
        Path::new(env!("CARGO_MANIFEST_DIR"))
    )
}

/// The value a probe reports for a measurement its own output never printed.
///
/// It is `u64::MAX` rather than zero because every bound this crate states about
/// a measurement is below it: a missing line then fails the bound it was going
/// to satisfy, instead of reading as the plausible small number a zero would
/// be. The sentinel is named rather than inlined so "a missing measurement
/// fails closed" is one fact in one place, and [`measurement`] is the only
/// place that decides it.
pub const MISSING_MEASUREMENT: u64 = u64::MAX;

/// The value of the `<label> <integer>` line named `label` in probe output, or
/// [`MISSING_MEASUREMENT`] when no line names it.
#[must_use]
pub fn measurement(stdout: &str, label: &str) -> u64 {
    let named = stdout
        .lines()
        .filter_map(|line| {
            let (name, value) = line.rsplit_once(' ')?;
            Some((name.trim(), value.trim().parse::<u64>().ok()?))
        })
        .find_map(|(name, value)| (name == label).then_some(value));
    match named {
        Some(value) => value,
        None => MISSING_MEASUREMENT,
    }
}

#[cfg(test)]
mod tests {
    use super::{MISSING_MEASUREMENT, manifest, measurement};

    /// Two measurements and the lines around them, in the shape a probe prints.
    const STDOUT: &str = "warm-match 1048576\ntokens-12 273\nnot-a-measurement\n";

    #[test]
    fn a_measurement_is_read_from_the_line_that_names_it() {
        assert_eq!(
            measurement(STDOUT, "warm-match"),
            1_048_576,
            "the value after a label is the measurement that label names"
        );
        assert_eq!(
            measurement(STDOUT, "tokens-12"),
            273,
            "a label is matched whole, not by its prefix"
        );
    }

    #[test]
    fn a_label_the_output_never_printed_fails_every_bound() {
        assert_eq!(
            measurement(STDOUT, "tokens-6"),
            MISSING_MEASUREMENT,
            "an unprinted measurement must read as the fail-closed value"
        );
        assert!(
            measurement(STDOUT, "tokens-6") > measurement(STDOUT, "tokens-12"),
            "a missing measurement must exceed every real one, or it would satisfy a bound"
        );
    }

    #[test]
    fn a_line_whose_value_is_not_an_integer_is_not_a_measurement() {
        assert_eq!(
            measurement(STDOUT, "not-a-measurement"),
            MISSING_MEASUREMENT,
            "a line whose last word is not an integer names no measurement"
        );
        assert_eq!(
            measurement("", "warm-match"),
            MISSING_MEASUREMENT,
            "empty output names no measurement"
        );
    }

    #[test]
    fn a_manifest_names_the_package_its_features_and_this_crate() {
        let plain = manifest("probe", &[]);
        assert!(
            plain.contains("name = \"probe\""),
            "the probe's package name must be in its manifest, got {plain:?}"
        );
        assert!(
            !plain.contains("features = ["),
            "a probe with no features must not declare a features clause, got {plain:?}"
        );
        assert!(
            plain.contains("default-features = false"),
            "a probe must declare default features off, or it measures the wrong crate, got {plain:?}"
        );

        let selected = manifest("probe", &["json", "wire"]);
        assert!(
            selected.contains("features = [\"json\", \"wire\"]"),
            "the selected features must be listed in order, got {selected:?}"
        );
    }
}
