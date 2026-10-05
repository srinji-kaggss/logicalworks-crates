//! Exercises JSON and RON as a downstream package sees the published facade.

use std::error::Error;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

/// A standalone consumer package derives, borrows, round-trips, and encodes
/// unsized values through each serialization feature independently.
#[test]
fn external_packages_use_json_ron_and_both_facades() -> Result<(), Box<dyn Error>> {
    let directory = tempfile::tempdir()?;
    let manifest_path = write_consumer(directory.path())?;
    fs::write(
        directory.path().join("src/main.rs"),
        consumer_source(true, false, false),
    )?;
    let lock = cargo_output(&manifest_path, &["generate-lockfile", "--offline"])?;
    assert!(
        lock.status.success(),
        "external consumer lock generation failed: {}",
        String::from_utf8_lossy(&lock.stderr)
    );

    for (features, source) in [
        ("json", consumer_source(true, false, false)),
        ("ron", consumer_source(false, true, false)),
        ("json,ron", consumer_source(true, true, false)),
    ] {
        fs::write(directory.path().join("src/main.rs"), source)?;
        let output = cargo_output(
            &manifest_path,
            &["run", "--locked", "--offline", "--features", features],
        )?;
        assert!(
            output.status.success(),
            "external {features} consumer failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fs::write(
        directory.path().join("src/main.rs"),
        consumer_source(false, true, true),
    )?;
    let broken_path = cargo_output(
        &manifest_path,
        &["check", "--locked", "--offline", "--features", "ron"],
    )?;
    let diagnostics = String::from_utf8_lossy(&broken_path.stderr);
    assert!(
        !broken_path.status.success(),
        "the deliberately broken derive path must fail"
    );
    assert!(
        diagnostics.contains("no `absent` in `ron`"),
        "the negative control must fail at the derive support path: {diagnostics}"
    );
    assert!(
        !diagnostics.contains("no matching package named"),
        "the negative control must not fail because a package is missing"
    );

    fs::write(
        directory.path().join("src/main.rs"),
        BROKEN_JSON_READER_CONSUMER,
    )?;
    let borrowed_reader = cargo_output(
        &manifest_path,
        &["check", "--locked", "--offline", "--features", "json"],
    )?;
    let diagnostics = String::from_utf8_lossy(&borrowed_reader.stderr);
    assert!(
        !borrowed_reader.status.success(),
        "reader decoding must reject output borrowing from its consumed buffer"
    );
    assert!(
        diagnostics.contains("not general enough") || diagnostics.contains("DeserializeOwned"),
        "the reader control must fail at its owned-output bound: {diagnostics}"
    );

    let tree = cargo_output(
        &manifest_path,
        &[
            "tree",
            "--locked",
            "--offline",
            "--features",
            "ron",
            "-i",
            "serde_json",
        ],
    )?;
    assert!(
        !tree.status.success(),
        "the RON-only consumer must not activate serde_json"
    );
    assert!(
        String::from_utf8_lossy(&tree.stderr).contains("did not match any packages"),
        "RON-only dependency inspection failed for an unexpected reason: {}",
        String::from_utf8_lossy(&tree.stderr)
    );
    Ok(())
}

/// Write a minimal independent Cargo package with no direct Serde dependency.
fn write_consumer(root: &Path) -> Result<std::path::PathBuf, Box<dyn Error>> {
    fs::create_dir(root.join("src"))?;
    let dependency_path = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest = format!(
        "[package]\nname = \"facade_consumer\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[features]\ndefault = []\njson = [\"lgwks_std/json\"]\nron = [\"lgwks_std/ron\"]\n\n[dependencies]\nlgwks_std = {{ path = {:?}, default-features = false }}\n",
        dependency_path
    );
    let manifest_path = root.join("Cargo.toml");
    fs::write(&manifest_path, manifest)?;
    Ok(manifest_path)
}

/// Capture one Cargo invocation while reusing the workspace build directory.
fn cargo_output(manifest_path: &Path, args: &[&str]) -> Result<Output, Box<dyn Error>> {
    let output = Command::new(env!("CARGO"))
        .args(args)
        .arg("--manifest-path")
        .arg(manifest_path)
        .env(
            "CARGO_TARGET_DIR",
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target"),
        )
        .output()?;
    Ok(output)
}

/// Build a consumer using the requested facade set and optional broken path.
fn consumer_source(json_enabled: bool, ron_enabled: bool, broken_path: bool) -> String {
    let mut source = String::from("fn main() -> Result<(), Box<dyn std::error::Error>> {\n");
    if json_enabled {
        source.push_str(JSON_CONSUMER);
    }
    if ron_enabled {
        if broken_path {
            source.push_str(BROKEN_RON_CONSUMER);
        } else {
            source.push_str(RON_CONSUMER);
        }
    }
    source.push_str("    Ok(())\n}\n");
    source
}

/// External JSON consumer exercises derive, borrowed output, owned round-trip,
/// and serialization of unsized strings and slices.
const JSON_CONSUMER: &str = r##"
mod json_consumer {
    use lgwks_std::json;

    #[derive(json::Serialize, json::Deserialize)]
    #[serde(crate = "lgwks_std::json::serde")]
    struct Borrowed<'a> { value: &'a str }
    #[derive(json::Serialize, json::Deserialize, PartialEq, Debug)]
    #[serde(crate = "lgwks_std::json::serde")]
    struct Owned { value: String }

    pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let text = r#"{"value":"borrowed"}"#;
    let borrowed: Borrowed<'_> = json::from_str(text)?;
    assert_eq!(borrowed.value, "borrowed");
    assert!(text.as_ptr() <= borrowed.value.as_ptr() && borrowed.value.as_ptr() < text.as_ptr().wrapping_add(text.len()));
    let bytes = text.as_bytes();
    let borrowed: Borrowed<'_> = json::from_slice(bytes)?;
    assert!(bytes.as_ptr() <= borrowed.value.as_ptr() && borrowed.value.as_ptr() < bytes.as_ptr().wrapping_add(bytes.len()));
    let escaped: Owned = json::from_str(r#"{"value":"line\nfeed"}"#)?;
    assert_eq!(escaped.value, "line\nfeed");
    let owned = Owned { value: String::from("owned") };
    let encoded = json::to_string(&owned)?;
    let restored: Owned = json::from_str(&encoded)?;
    assert_eq!(owned, restored);
    let reader_value: Owned = json::from_reader(std::io::Cursor::new(encoded.as_bytes()))?;
    assert_eq!(owned, reader_value);
    assert_eq!(json::to_string("word")?, "\"word\"");
    assert_eq!(json::to_string([1, 2].as_slice())?, "[1,2]");
        Ok(())
    }
}
json_consumer::run()?;
"##;

/// External RON consumer exercises the same operations without enabling JSON.
const RON_CONSUMER: &str = r#"
mod ron_consumer {
    use lgwks_std::ron;

    #[derive(ron::Serialize, ron::Deserialize)]
    #[serde(crate = "lgwks_std::ron::serde")]
    struct Borrowed<'a> { value: &'a str }
    #[derive(ron::Serialize, ron::Deserialize, PartialEq, Debug)]
    #[serde(crate = "lgwks_std::ron::serde")]
    struct Owned { value: String }

    pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let text = "(value: \"borrowed\")";
    let borrowed: Borrowed<'_> = ron::from_str(text)?;
    assert_eq!(borrowed.value, "borrowed");
    assert!(text.as_ptr() <= borrowed.value.as_ptr() && borrowed.value.as_ptr() < text.as_ptr().wrapping_add(text.len()));
    let bytes = text.as_bytes();
    let borrowed: Borrowed<'_> = ron::from_slice(bytes)?;
    assert!(bytes.as_ptr() <= borrowed.value.as_ptr() && borrowed.value.as_ptr() < bytes.as_ptr().wrapping_add(bytes.len()));
    let escaped: Owned = ron::from_str("(value: \"line\\nfeed\")")?;
    assert_eq!(escaped.value, "line\nfeed");
    let owned = Owned { value: String::from("owned") };
    let encoded = ron::to_string(&owned)?;
    let restored: Owned = ron::from_str(&encoded)?;
    assert_eq!(owned, restored);
    assert_eq!(ron::to_string("word")?, "\"word\"");
    assert_eq!(ron::to_string([1, 2].as_slice())?, "[1,2]");
        Ok(())
    }
}
ron_consumer::run()?;
"#;

/// Deliberately broken facade path verifies the compiler catches the intended
/// derive-path defect while all required crates are present.
const BROKEN_RON_CONSUMER: &str = r#"
mod broken_ron_consumer {
    use lgwks_std::ron;
    #[derive(ron::Serialize)]
    #[serde(crate = "lgwks_std::ron::absent")]
    struct Broken { value: String }
}
"#;

/// A reader cannot return a reference into its internally consumed buffer.
const BROKEN_JSON_READER_CONSUMER: &str = r##"
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use lgwks_std::json;
    #[derive(json::Deserialize)]
    #[serde(crate = "lgwks_std::json::serde")]
    struct Borrowed<'a> { value: &'a str }
    let text = r#"{"value":"reader"}"#;
    let _: Borrowed<'_> = json::from_reader(std::io::Cursor::new(text))?;
    Ok(())
}
"##;
