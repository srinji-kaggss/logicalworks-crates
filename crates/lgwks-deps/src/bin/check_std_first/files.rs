//! Which files the audit reads: the repository's, not whatever is on disk.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use super::{AuditError, MAX_DEPTH, fail};

/// Where source lives. `src` is the shipped surface; the rest are compiled by
/// `--all-targets` and are held to the same rule, because a test that reaches a
/// crate its manifest does not declare is the same unregistered edge.
const SOURCE_DIRECTORIES: [&str; 4] = ["src", "tests", "examples", "benches"];

/// The files git says the repository holds, or `None` outside a checkout.
///
/// Tracked files plus untracked ones `.gitignore` does not exclude: a new file is
/// checked before it is staged, and build output is not. `None`, not an empty
/// set, when git cannot answer, because "no files" would pass every check.
pub(super) fn repository_files(repo: &Path) -> Option<BTreeSet<PathBuf>> {
    let listing = match Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .output()
    {
        Ok(listing) => listing,
        Err(error) => {
            lgwks_std::trace::debug!(%error, "git could not be run; the disk is the only view");
            return None;
        }
    };
    if !listing.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&listing.stdout)
            .split('\0')
            .filter(|name| !name.is_empty())
            .map(|name| repo.join(name))
            .collect(),
    )
}

/// Every `.rs` file under `directory`, recursively, added to `out`.
fn rust_files_under(
    directory: &Path,
    depth: usize,
    out: &mut BTreeSet<PathBuf>,
) -> Result<(), AuditError> {
    if depth > MAX_DEPTH {
        return fail(
            AuditError::TooDeep {
                path: directory.to_path_buf(),
            },
            "rust_files_under",
        );
    }
    let reader = match fs::read_dir(directory) {
        Ok(reader) => reader,
        Err(error) => {
            lgwks_std::trace::debug!(%error, path = ?directory, "a source directory is absent");
            return Ok(());
        }
    };
    for entry in reader {
        let entry = match entry {
            Ok(entry) => entry,
            Err(cause) => {
                return fail(
                    AuditError::Io {
                        path: directory.to_path_buf(),
                        cause,
                    },
                    "rust_files_under",
                );
            }
        };
        let path = entry.path();
        if path.is_dir() {
            rust_files_under(&path, depth.saturating_add(1), out)?;
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            out.insert(path);
        }
    }
    Ok(())
}

/// Every source file of the repository that a source directory names.
///
/// The disk alone holds more than the repository: a fixture compiled in place
/// leaves `target/debug/build/*/out/*.rs` under `crates/*/tests/`, and this gate
/// failed on one machine and passed on another over two such generated files.
/// Intersecting with what git holds makes the verdict a property of the
/// repository. Outside a git checkout the disk is the only view.
pub(super) fn source_files(repo: &Path) -> Result<Vec<PathBuf>, AuditError> {
    let held = repository_files(repo);
    let mut matched = BTreeSet::new();
    let crates = repo.join("crates");
    let members = match fs::read_dir(&crates) {
        Ok(reader) => reader,
        Err(error) => {
            lgwks_std::trace::debug!(%error, path = ?crates, "there is no crates directory");
            return Ok(Vec::new());
        }
    };
    for member in members {
        let member = match member {
            Ok(member) => member,
            Err(cause) => {
                return fail(
                    AuditError::Io {
                        path: crates,
                        cause,
                    },
                    "source_files",
                );
            }
        };
        for directory in SOURCE_DIRECTORIES {
            rust_files_under(&member.path().join(directory), 0, &mut matched)?;
        }
    }
    Ok(match held {
        Some(held) => matched.intersection(&held).cloned().collect(),
        None => matched.into_iter().collect(),
    })
}

/// The file's text with comment-only lines blanked, newlines preserved.
///
/// A pattern quoted inside a comment is documentation about the pattern, not an
/// instance of it. Blanking rather than deleting keeps every offset on its own
/// line, so a reported line number is the one a reader opens.
pub(super) fn cleaned_text(path: &Path) -> Result<String, AuditError> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(cause) => {
            return fail(
                AuditError::Io {
                    path: path.to_path_buf(),
                    cause,
                },
                "cleaned_text",
            );
        }
    };
    Ok(text
        .lines()
        .map(|line| {
            if line.trim_start().starts_with("//") {
                ""
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

/// The crate a source file belongs to, from its directory under `crates/`.
///
/// Keyed on the directory and not on the package name: the two are equal in this
/// workspace once dashes are normalised, which is exactly why reading the package
/// name off the path looks right and is not.
pub(super) fn owning_crate<'a>(
    repo: &Path,
    path: &Path,
    by_directory: &'a BTreeMap<String, String>,
) -> Option<&'a String> {
    let relative = path.strip_prefix(repo).ok()?;
    let parts: Vec<String> = relative
        .components()
        .filter_map(|part| match part {
            Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
            Component::Prefix(_)
            | Component::RootDir
            | Component::CurDir
            | Component::ParentDir => None,
        })
        .collect();
    if parts.len() < 2 || parts.first().map(String::as_str) != Some("crates") {
        return None;
    }
    by_directory.get(parts.get(1)?)
}

/// A directory a test owns, claimed with an exclusive create so concurrent test
/// processes never share one.
#[cfg(test)]
pub(super) struct Scratch {
    /// The directory.
    pub(super) root: PathBuf,
}

#[cfg(test)]
impl Scratch {
    /// Claims the first free `lgwks-std-first-<n>` under the system temp directory.
    pub(super) fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let base = std::env::temp_dir();
        for attempt in 0..4096_u32 {
            let root = base.join(format!("lgwks-std-first-{attempt}"));
            match fs::create_dir(&root) {
                Ok(()) => return Ok(Self { root }),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(Box::new(error)),
            }
        }
        Err("no free scratch directory".into())
    }
}

#[cfg(test)]
impl Drop for Scratch {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.root) {
            lgwks_std::trace::debug!(%error, "a scratch directory could not be removed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Scratch, cleaned_text, owning_crate, source_files};
    use std::collections::BTreeMap;
    use std::error::Error;
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    type TestResult = Result<(), Box<dyn Error>>;

    #[test]
    fn ignored_build_output_is_not_source_and_a_new_file_is() -> TestResult {
        let scratch = Scratch::new()?;
        let repo = &scratch.root;
        if !Command::new("git")
            .arg("init")
            .arg("-q")
            .arg(repo)
            .status()?
            .success()
        {
            return Err("git init failed".into());
        }
        fs::write(repo.join(".gitignore"), "target/\n")?;
        let kept = repo.join("crates/demo/tests/unstaged.rs");
        let generated =
            repo.join("crates/demo/tests/fixtures/app/target/debug/build/out/private.rs");
        for path in [&kept, &generated] {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(path, "use serde_core::Serialize;\n")?;
        }
        assert_eq!(
            source_files(repo)?,
            vec![kept],
            "only the repository's own file"
        );
        Ok(())
    }

    #[test]
    fn a_comment_only_line_is_blanked_and_the_line_count_kept() -> TestResult {
        let scratch = Scratch::new()?;
        let path = scratch.root.join("a.rs");
        fs::write(&path, "let a = 1;\n  // use serde::Value;\nlet b = 2;\n")?;
        assert_eq!(
            cleaned_text(&path)?,
            "let a = 1;\n\nlet b = 2;",
            "the comment line is empty"
        );
        Ok(())
    }

    #[test]
    fn a_source_file_belongs_to_the_crate_of_its_directory() {
        let by_directory: BTreeMap<String, String> = [("fake".to_owned(), "lgwks_fake".to_owned())]
            .into_iter()
            .collect();
        let repo = Path::new("/repo");
        assert_eq!(
            owning_crate(
                repo,
                Path::new("/repo/crates/fake/src/lib.rs"),
                &by_directory
            )
            .map(String::as_str),
            Some("lgwks_fake"),
            "the directory names the crate"
        );
        assert_eq!(
            owning_crate(repo, Path::new("/repo/scripts/x.rs"), &by_directory),
            None,
            "a file outside crates/ belongs to no member"
        );
    }
}
