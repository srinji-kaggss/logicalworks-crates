//! `build` gives a Cargo build script the two things it needs to bind what it
//! generates to what it read. An [`InputDigest`](crate::build::InputDigest) is a
//! function of its domain
//! tag, the bytes and root-relative names of its input files, its build
//! variables and the enabled cargo features, and of nothing else, so two
//! checkouts of one tree at different paths digest equal; the seeded family
//! `tests/it/sim_build.rs` holds each part of that.
//!
//! The digest is BLAKE3 through [`crate::hash::Hasher::write_framed`]: each
//! record is a framed tag (`file`, `var`, `feature`) followed by its framed
//! parts, so no two input sets feed the same byte stream. Trees are walked by
//! [`crate::fs::walk_dir_entries`], strict and depth-bounded, and only regular
//! files are inputs; a symlink is neither followed nor hashed.
//!
//! Cargo reads a build script's instructions from its stdout.
//! [`directive`](crate::build::directive)
//! writes one `cargo:` line to the writer it is given and returns the write
//! error, where `println!` would panic on a closed pipe; a build script passes
//! `&mut std::io::stdout().lock()`. Failures are also emitted as `debug!`
//! events through [`crate::trace`], for a build script that installs a
//! subscriber.

use std::env;
use std::error::Error;
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::fs::{Descend, FileKind, WalkEntry, WalkOptions, walk_dir_entries};
use crate::hash::{Digest, Hasher};
use crate::trace::debug;

/// Prefix of the cargo variables that name the enabled features.
const FEATURE_PREFIX: &str = "CARGO_FEATURE_";

/// Why a build step failed.
///
/// `#[non_exhaustive]` so a later revision can add a refusal without breaking
/// callers that match on these.
#[derive(Debug)]
#[non_exhaustive]
pub enum BuildError {
    /// An input file, tree or root could not be read or resolved.
    Read {
        /// The path that failed.
        path: PathBuf,
        /// The operating system's error.
        source: io::Error,
    },
    /// An input lies outside the digest's root, so it has no portable name.
    OutsideRoot {
        /// The input that is not under the root.
        path: PathBuf,
    },
    /// An input's name below the root is not UTF-8, so it has no portable name.
    PathNotUnicode {
        /// The input whose name cannot be spelled.
        path: PathBuf,
    },
    /// A variable cargo sets for every build script is missing.
    Unset {
        /// The variable's name.
        name: String,
    },
    /// A build variable is set to a value that is not UTF-8.
    NotUnicode {
        /// The variable's name.
        name: String,
    },
    /// Writing a directive to cargo failed.
    Directive(io::Error),
}

impl fmt::Display for BuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Read {
                ref path,
                ref source,
            } => {
                write!(
                    formatter,
                    "cannot read build input {}: {source}",
                    path.display()
                )
            }
            Self::OutsideRoot { ref path } => {
                write!(
                    formatter,
                    "build input {} is outside the digest root",
                    path.display()
                )
            }
            Self::PathNotUnicode { ref path } => {
                write!(
                    formatter,
                    "build input {} has a name that is not UTF-8",
                    path.display()
                )
            }
            Self::Unset { ref name } => write!(formatter, "cargo did not set {name}"),
            Self::NotUnicode { ref name } => {
                write!(formatter, "build variable {name} is not UTF-8")
            }
            Self::Directive(ref source) => {
                write!(formatter, "cannot write a cargo directive: {source}")
            }
        }
    }
}

impl Error for BuildError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match *self {
            Self::Read { ref source, .. } | Self::Directive(ref source) => Some(source),
            Self::OutsideRoot { .. }
            | Self::PathNotUnicode { .. }
            | Self::Unset { .. }
            | Self::NotUnicode { .. } => None,
        }
    }
}

/// Writes one `cargo:` instruction, such as `rustc-link-lib=framework=Security`,
/// to `out` as its own line.
///
/// # Errors
///
/// [`BuildError::Directive`] when the write fails, for example on a closed pipe.
pub fn directive(out: &mut dyn Write, instruction: &str) -> Result<(), BuildError> {
    writeln!(out, "cargo:{instruction}").map_err(BuildError::Directive)
}

/// The directory cargo gives this build script for generated files (`OUT_DIR`).
///
/// # Errors
///
/// [`BuildError::Unset`] outside a build script.
pub fn out_dir() -> Result<PathBuf, BuildError> {
    cargo_path("OUT_DIR")
}

/// The package directory of the crate being built (`CARGO_MANIFEST_DIR`).
///
/// # Errors
///
/// [`BuildError::Unset`] outside a build script.
pub fn manifest_dir() -> Result<PathBuf, BuildError> {
    cargo_path("CARGO_MANIFEST_DIR")
}

/// Reads one path-valued variable cargo sets for every build script.
fn cargo_path(name: &str) -> Result<PathBuf, BuildError> {
    if let Some(value) = env::var_os(name) {
        return Ok(PathBuf::from(value));
    }
    let refusal = BuildError::Unset {
        name: name.to_owned(),
    };
    debug!(error = %refusal, "cargo_path: returning an error to the caller");
    Err(refusal)
}

/// The inputs of one build step, collected under one root, digested together.
///
/// Files and trees are named relative to the root, so the digest does not
/// depend on where the checkout lives.
#[derive(Debug)]
#[non_exhaustive]
pub struct InputDigest {
    /// Domain tag that separates this digest from every other use of BLAKE3.
    domain: String,
    /// Canonical root every input is named against.
    root: PathBuf,
    /// Directories walked, re-announced to cargo so a new file reruns the script.
    trees: Vec<PathBuf>,
    /// Every input file, absolute.
    files: Vec<PathBuf>,
    /// Build variables, in the order given.
    variables: Vec<String>,
}

impl InputDigest {
    /// Starts a digest tagged `domain` whose inputs live under `root`.
    ///
    /// # Errors
    ///
    /// [`BuildError::Read`] when `root` cannot be resolved.
    pub fn new(domain: &str, root: impl AsRef<Path>) -> Result<Self, BuildError> {
        Ok(Self {
            domain: domain.to_owned(),
            root: canonical(root.as_ref())?,
            trees: Vec::new(),
            files: Vec::new(),
            variables: Vec::new(),
        })
    }

    /// Adds one file; a relative path is taken from the root.
    pub fn file(&mut self, path: impl AsRef<Path>) {
        self.files.push(self.root.join(path));
    }

    /// Adds every regular file under `directory` (relative paths are taken from
    /// the root), walked strictly to the default depth bound.
    ///
    /// # Errors
    ///
    /// [`BuildError::Read`] when the directory or any entry under it cannot be read.
    pub fn tree(&mut self, directory: impl AsRef<Path>) -> Result<(), BuildError> {
        let directory = canonical(&self.root.join(directory))?;
        let walked = walk_dir_entries(&directory, &WalkOptions::default(), None, |_, _| {
            Descend::Enter
        });
        let entries = match walked {
            Ok(entries) => entries,
            Err(source) => {
                let refusal = BuildError::Read {
                    path: directory,
                    source,
                };
                debug!(error = %refusal, "tree: returning an error to the caller");
                return Err(refusal);
            }
        };
        self.files.extend(
            entries
                .into_iter()
                .filter(|entry| entry.kind() == FileKind::File)
                .map(WalkEntry::into_path),
        );
        self.trees.push(directory);
        Ok(())
    }

    /// Adds one build variable, such as `TARGET` or `CARGO_ENCODED_RUSTFLAGS`.
    /// An unset variable digests as empty; cargo leaves optional ones unset.
    pub fn variable(&mut self, name: &str) {
        self.variables.push(name.to_owned());
    }

    /// Digests every input and every enabled cargo feature, announcing each file,
    /// tree and variable to cargo on `directives` so a change reruns the script.
    ///
    /// # Errors
    ///
    /// [`BuildError::OutsideRoot`] or [`BuildError::PathNotUnicode`] for an input
    /// with no portable name, [`BuildError::Read`] for an unreadable one,
    /// [`BuildError::NotUnicode`] for a variable, and [`BuildError::Directive`]
    /// when `directives` refuses a write.
    pub fn finish(&self, directives: &mut dyn Write) -> Result<Digest, BuildError> {
        let mut hasher = Hasher::new();
        hasher.write_framed(self.domain.as_bytes());
        for tree in &self.trees {
            directive(directives, &format!("rerun-if-changed={}", tree.display()))?;
        }
        let mut files = self.files.clone();
        files.sort_unstable();
        files.dedup();
        for path in &files {
            directive(directives, &format!("rerun-if-changed={}", path.display()))?;
            let name = self.portable_name(path)?;
            let bytes = read_input(path)?;
            hasher
                .write_framed(b"file")
                .write_framed(name.as_bytes())
                .write_framed(&bytes);
        }
        for name in &self.variables {
            directive(directives, &format!("rerun-if-env-changed={name}"))?;
            let value = variable_value(name)?;
            hasher
                .write_framed(b"var")
                .write_framed(name.as_bytes())
                .write_framed(value.as_bytes());
        }
        let mut features: Vec<(String, String)> = env::vars()
            .filter(|pair| pair.0.starts_with(FEATURE_PREFIX))
            .collect();
        features.sort_unstable();
        for pair in &features {
            hasher
                .write_framed(b"feature")
                .write_framed(pair.0.as_bytes())
                .write_framed(pair.1.as_bytes());
        }
        Ok(hasher.finalize())
    }

    /// The input's name below the root, `/`-separated on every platform.
    fn portable_name(&self, path: &Path) -> Result<String, BuildError> {
        let Ok(relative) = path.strip_prefix(&self.root) else {
            let refusal = BuildError::OutsideRoot {
                path: path.to_path_buf(),
            };
            debug!(error = %refusal, "portable_name: returning an error to the caller");
            return Err(refusal);
        };
        let mut parts = Vec::new();
        for component in relative.components() {
            let Some(part) = component.as_os_str().to_str() else {
                let refusal = BuildError::PathNotUnicode {
                    path: path.to_path_buf(),
                };
                debug!(error = %refusal, "portable_name: returning an error to the caller");
                return Err(refusal);
            };
            parts.push(part);
        }
        Ok(parts.join("/"))
    }
}

/// Reads one input file whole.
fn read_input(path: &Path) -> Result<Vec<u8>, BuildError> {
    match fs::read(path) {
        Ok(bytes) => Ok(bytes),
        Err(source) => {
            let refusal = BuildError::Read {
                path: path.to_path_buf(),
                source,
            };
            debug!(error = %refusal, "read_input: returning an error to the caller");
            Err(refusal)
        }
    }
}

/// A build variable's value; unset is empty, and a non-UTF-8 value is refused.
fn variable_value(name: &str) -> Result<String, BuildError> {
    match env::var(name) {
        Ok(value) => Ok(value),
        Err(env::VarError::NotPresent) => Ok(String::new()),
        Err(env::VarError::NotUnicode(_)) => {
            let refusal = BuildError::NotUnicode {
                name: name.to_owned(),
            };
            debug!(error = %refusal, "variable_value: returning an error to the caller");
            Err(refusal)
        }
    }
}

/// Resolves `path` to its canonical form, so names are taken against one spelling.
fn canonical(path: &Path) -> Result<PathBuf, BuildError> {
    match path.canonicalize() {
        Ok(resolved) => Ok(resolved),
        Err(source) => {
            let refusal = BuildError::Read {
                path: path.to_path_buf(),
                source,
            };
            debug!(error = %refusal, "canonical: returning an error to the caller");
            Err(refusal)
        }
    }
}
