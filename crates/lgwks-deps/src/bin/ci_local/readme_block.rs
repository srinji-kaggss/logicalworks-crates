//! The publisher block every crate README carries, and the front page's link to
//! `llms.txt` (issues #718 and #719), read by the `contract-drift` lane.
//!
//! crates.io and docs.rs serve a crate's packaged README and nothing else from the
//! repository, so that page is the one place a reader, or an answer engine, meets a
//! crate with no repository around it. Without the block it can name the crate and
//! its licence and not who publishes it or that four sibling crates exist. A
//! packaged README also has no base URL, so a relative link breaks there: the block
//! links by absolute URL, and the check refuses a relative one.
//!
//! The block is judged on its words with whitespace collapsed, so a paragraph that
//! is rewrapped is the same paragraph, and on a bounded window after its opening
//! sentence, so its pieces cannot be scattered through a README and still count.

use std::path::Path;

use super::builtins::{crate_directories, relative, tail};
use super::{GateError, read};

/// The crates the block names, in the order it names them. A crate directory with
/// a README that is not here is refused, so a sixth crate cannot ship without the
/// block naming it.
const CRATES: [&str; 5] = [
    "lgwks_std",
    "lgwks_bot",
    "lgwks_ast",
    "lgwks_macros",
    "lgwks_deps",
];

/// The repository's README, as the block links it.
const REPOSITORY_README: &str = "https://github.com/srinji-kaggss/logicalworks-crates#readme";

/// `docs/should-i-use.md`, as the block links it.
const SHOULD_I_USE: &str =
    "https://github.com/srinji-kaggss/logicalworks-crates/blob/HEAD/docs/should-i-use.md";

/// The markdown link the front page carries to the agent-readable index.
const LLMS_LINK: &str = "](llms.txt)";

/// How many characters after the opening words the rest of the block must fall in.
const WINDOW: usize = 700;

/// `text` with every run of whitespace as one space, so a wrapped paragraph reads
/// as one line.
fn flat(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// What `readme`, the README of crate `name`, lacks of the publisher block: the
/// first missing piece, or `None` when the block is whole.
fn publisher_gap(readme: &str, name: &str) -> Option<String> {
    let words = flat(readme);
    let opening = format!("Logical Works Incorporated publishes `{name}` as one of five");
    let Some(at) = words.find(&opening) else {
        return Some(format!("no sentence beginning {opening:?}"));
    };
    let window: String = tail(&words, at.saturating_add(opening.len()))
        .chars()
        .take(WINDOW)
        .collect();
    let links = [
        format!("]({REPOSITORY_README})"),
        format!("]({SHOULD_I_USE})"),
    ];
    CRATES
        .iter()
        .map(|listed| format!("`{listed}`"))
        .chain(links)
        .find(|needle| !window.contains(needle.as_str()))
        .map(|needle| format!("the publisher block lacks {needle:?}"))
}

/// What the repository front page lacks of the link to `llms.txt`.
fn front_page_gap(readme: &str) -> Option<String> {
    if flat(readme).contains(LLMS_LINK) {
        None
    } else {
        Some(format!(
            "no markdown link {LLMS_LINK:?} to the agent-readable index"
        ))
    }
}

/// Whether `directory` is one of the crates the block names.
fn names_a_crate(directory: &Path) -> bool {
    directory.file_name().is_some_and(|stem| {
        let spelled = stem.to_string_lossy().replace('-', "_");
        CRATES.contains(&spelled.as_str())
    })
}

/// Every way the tree's READMEs fall short of the block, one line each.
pub(super) fn gaps(root: &Path) -> Result<Vec<String>, GateError> {
    let mut found = Vec::new();
    for name in CRATES {
        let path = root
            .join("crates")
            .join(name.replace('_', "-"))
            .join("README.md");
        if let Some(gap) = publisher_gap(&read(&path)?, name) {
            found.push(format!("{}: {gap}", relative(root, &path)));
        }
    }
    for directory in crate_directories(root) {
        let readme = directory.join("README.md");
        if readme.is_file() && !names_a_crate(&directory) {
            found.push(format!(
                "{}: a crate the publisher block does not name; add it to the block and to CRATES",
                relative(root, &readme)
            ));
        }
    }
    let front = root.join("README.md");
    if let Some(gap) = front_page_gap(&read(&front)?) {
        found.push(format!("{}: {gap}", relative(root, &front)));
    }
    Ok(found)
}

#[cfg(test)]
mod sim_readme_block;

#[cfg(test)]
mod tests {
    use std::fmt;
    use std::fs;
    use std::io;
    use std::path::{Path, PathBuf};

    use super::{CRATES, gaps};
    use crate::GateError;
    use crate::fixture::Fixture;

    /// Why a tree test could not run: the scratch tree or the check itself failed.
    #[derive(Debug)]
    enum TreeError {
        /// The scratch tree could not be written.
        Io(io::Error),
        /// The check could not read the tree.
        Gate(GateError),
    }

    impl fmt::Display for TreeError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            match *self {
                Self::Io(ref error) => write!(formatter, "the scratch tree: {error}"),
                Self::Gate(ref error) => write!(formatter, "the check: {error}"),
            }
        }
    }

    impl std::error::Error for TreeError {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            match *self {
                Self::Io(ref error) => Some(error),
                Self::Gate(ref error) => Some(error),
            }
        }
    }

    impl From<io::Error> for TreeError {
        fn from(error: io::Error) -> Self {
            Self::Io(error)
        }
    }

    impl From<GateError> for TreeError {
        fn from(error: GateError) -> Self {
            Self::Gate(error)
        }
    }

    type TestResult = Result<(), TreeError>;

    /// The block of crate `name`, as a README carries it.
    fn block_of(name: &str) -> String {
        format!(
            "Logical Works Incorporated publishes `{name}` as one of five independently versioned crates in [logicalworks-crates](https://github.com/srinji-kaggss/logicalworks-crates#readme): `lgwks_std`, `lgwks_bot`, `lgwks_ast`, `lgwks_macros` and `lgwks_deps`.\n[Should I use logicalworks-crates?](https://github.com/srinji-kaggss/logicalworks-crates/blob/HEAD/docs/should-i-use.md) says when to reach for each one and when to skip it.\n"
        )
    }

    /// A tree path under `root`, written with its directories.
    fn put(root: &Path, relative_path: &str, text: &str) -> TestResult {
        let path = root.join(relative_path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, text)?;
        Ok(())
    }

    /// The repository this binary was built from.
    fn repo() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    /// Proves: the READMEs in this very tree hold the block, which is what the lane
    /// runs; a tree that fails here fails the gate.
    #[test]
    fn the_repository_readmes_hold_the_block() -> TestResult {
        let found = gaps(&repo())?;
        assert!(found.is_empty(), "{}", found.join("\n"));
        Ok(())
    }

    /// Proves: each of the three ways a tree can fail is reported against its own
    /// file: a crate whose README lost the block, a crate directory the block does
    /// not name, and a front page without the link. A directory with no README is
    /// not a crate here.
    #[test]
    fn the_tree_check_names_each_readme_that_fails_it() -> TestResult {
        let tree = Fixture::claim(&std::env::temp_dir(), "lgwks-readme-block")?;
        for name in CRATES {
            let path = format!("crates/{}/README.md", name.replace('_', "-"));
            put(tree.path(), &path, &block_of(name))?;
        }
        put(
            tree.path(),
            "README.md",
            "Agent-readable index: [`llms.txt`](llms.txt).\n",
        )?;
        put(tree.path(), "crates/no-readme/Cargo.toml", "[package]\n")?;
        let clean = gaps(tree.path())?;
        assert!(clean.is_empty(), "a complete tree is clean: {clean:?}");
        put(
            tree.path(),
            "crates/lgwks-extra/README.md",
            "# lgwks_extra\n",
        )?;
        put(tree.path(), "crates/lgwks-bot/README.md", "# lgwks_bot\n")?;
        put(tree.path(), "README.md", "# logicalworks-crates\n")?;
        let found = gaps(tree.path())?;
        for file in [
            "crates/lgwks-extra/README.md",
            "crates/lgwks-bot/README.md",
            "README.md",
        ] {
            let own = found
                .iter()
                .filter(|line| line.starts_with(&format!("{file}: ")))
                .count();
            assert_eq!(own, 1, "one line for {file} in {found:?}");
        }
        assert_eq!(found.len(), 3, "nothing else is reported: {found:?}");
        Ok(())
    }
}
