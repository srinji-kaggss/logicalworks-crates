//! The manifest reader over the shapes a normalised `Cargo.toml` takes, and
//! the cache walk over a registry laid out as Cargo lays it out.

use std::error::Error;
use std::path::{Path, PathBuf};

use lgwks_std::{hex, random};

use super::{cached, repository_of};
use crate::freshness_cmd::SimError;

/// What every test here returns.
type TestResult = Result<(), Box<dyn Error>>;

/// A directory under the OS temp root, removed when the test ends.
///
/// The name carries 64 random bits, so two tests never share a directory and a
/// leftover from an earlier run is never read: process ids are reused by the
/// OS, so they are not a name.
struct Scratch(PathBuf);

impl Scratch {
    /// A new empty directory named for the test.
    fn new(tag: &str) -> Result<Self, SimError> {
        let nonce = hex::encode(random::bytes::<8>()?);
        let path = std::env::temp_dir().join(format!("lgwks-deps-repository-{tag}-{nonce}"));
        std::fs::create_dir_all(&path)?;
        Ok(Self(path))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if std::fs::remove_dir_all(&self.0).is_err() {
            // Nothing to report here; `Drop` has no failure channel.
        }
    }
}

/// Lays `manifest` down as Cargo unpacks `name` at `version` under `index`.
fn unpack(sources: &Path, index: &str, name: &str, version: &str, manifest: &str) -> TestResult {
    let directory = sources.join(index).join(format!("{name}-{version}"));
    std::fs::create_dir_all(&directory)?;
    std::fs::write(directory.join("Cargo.toml"), manifest)?;
    Ok(())
}

#[test]
fn the_repository_is_read_from_the_package_table_only() {
    let manifest = "[package]\nname = \"serde\"\nversion = \"1.0.0\"\n\
                    repository = \"https://github.com/serde-rs/serde\"\n\
                    [dependencies]\nrepository = \"https://elsewhere.test\"\n";
    assert_eq!(
        repository_of(manifest),
        Some("https://github.com/serde-rs/serde".to_owned()),
        "the package key wins"
    );
    let elsewhere = "[dependencies.x]\nrepository = \"https://elsewhere.test\"\n";
    assert_eq!(
        repository_of(elsewhere),
        None,
        "another table is not the package"
    );
}

#[test]
fn a_key_that_only_starts_like_repository_is_not_it() {
    for manifest in [
        "[package]\nrepository-url = \"https://x.test\"\n",
        "[package]\nrepository.workspace = true\n",
        "[package]\nrepositoryx = \"https://x.test\"\n",
        "[package]\ndocumentation = \"https://x.test\"\n",
        "[package]\n",
        "",
    ] {
        assert_eq!(
            repository_of(manifest),
            None,
            "{manifest:?} names no repository"
        );
    }
}

#[test]
fn a_value_that_is_not_exactly_what_the_publisher_wrote_is_refused() {
    for manifest in [
        "[package]\nrepository = \"\"\n",
        "[package]\nrepository = \"https://x.test/a\\\\b\"\n",
        "[package]\nrepository = \"https://x.test\" trailing\n",
        "[package]\nrepository = \"unterminated\n",
        "[package]\nrepository = 'single'\n",
        "[package]\nrepository = true\n",
    ] {
        assert_eq!(
            repository_of(manifest),
            None,
            "{manifest:?} is not a plain string"
        );
    }
    assert_eq!(
        repository_of("[package]\nrepository = \"https://x.test\" # the home\n"),
        Some("https://x.test".to_owned()),
        "a trailing comment is not part of the value"
    );
    assert_eq!(
        repository_of("[package]\n  repository   =   \"https://x.test\"  \r\n"),
        Some("https://x.test".to_owned()),
        "spacing and CRLF are not part of the value"
    );
}

#[test]
fn the_cache_is_read_for_the_resolved_version_across_every_index() -> TestResult {
    let scratch = Scratch::new("indexes")?;
    let sources = scratch.0.as_path();
    unpack(
        sources,
        "index.crates.io-aaaa",
        "serde",
        "1.0.0",
        "[package]\nrepository = \"https://old.test/serde\"\n",
    )?;
    unpack(
        sources,
        "index.crates.io-aaaa",
        "serde",
        "1.0.1",
        "[package]\nrepository = \"https://new.test/serde\"\n",
    )?;
    unpack(
        sources,
        "github.com-bbbb",
        "tokio",
        "1.0.0",
        "[package]\nrepository = \"https://github.com/tokio-rs/tokio\"\n",
    )?;
    assert_eq!(
        cached(sources, "serde", "1.0.0"),
        Some("https://old.test/serde".to_owned()),
        "the resolved version, not the newest unpacked"
    );
    assert_eq!(
        cached(sources, "tokio", "1.0.0"),
        Some("https://github.com/tokio-rs/tokio".to_owned()),
        "an older index directory is searched too"
    );
    assert_eq!(cached(sources, "serde", "9.9.9"), None, "not unpacked here");
    assert_eq!(
        cached(sources, "absent", "1.0.0"),
        None,
        "not a known crate"
    );
    Ok(())
}

#[test]
fn two_indexes_holding_one_package_answer_in_name_order() -> TestResult {
    let scratch = Scratch::new("order")?;
    let sources = scratch.0.as_path();
    for (index, repository) in [("zzz", "https://z.test"), ("aaa", "https://a.test")] {
        unpack(
            sources,
            index,
            "dup",
            "1.0.0",
            &format!("[package]\nrepository = \"{repository}\"\n"),
        )?;
    }
    assert_eq!(
        cached(sources, "dup", "1.0.0"),
        Some("https://a.test".to_owned()),
        "the first index by name answers, whatever order the disk lists them"
    );
    Ok(())
}

#[test]
fn a_missing_cache_is_no_answer_not_a_failure() {
    let nowhere = Path::new("/nonexistent/lgwks-deps-registry-src");
    assert_eq!(
        cached(nowhere, "serde", "1.0.0"),
        None,
        "no registry directory"
    );
}
