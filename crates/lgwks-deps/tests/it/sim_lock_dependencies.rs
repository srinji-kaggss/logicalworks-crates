//! The `dependencies` array of `Cargo.lock`, read in every spelling TOML allows.
//!
//! `freshness` names the direct dependency each package comes through (#702), so
//! the reader now keeps each package's `dependencies`. An edge the reader loses
//! is a package attributed to the wrong release, and one it invents is a release
//! the reader is told to wait for that does not matter. Cargo writes one
//! spelling: one quoted entry per line, a trailing comma. A lock is a file
//! anyone can edit, and TOML (v1.0.0, `toml.abnf`) allows more: the array on one
//! line, literal strings, comments and blank lines between entries, several
//! entries per line, a quoted key, an empty array on one line or several.
//!
//! One seed draws the packages, their entries, and the spelling of each array;
//! the reader must return exactly the drawn entries, in order, and the array of
//! one package must never leak into the next. Entries carry the characters real
//! locks carry: spaces, parentheses, and the `#` of a git revision, which is
//! inside a quoted string and so is not a comment. The refusals are checked at
//! the line the reader is documented to name.

use lgwks_deps::lock::{self, LockError};

use crate::replay::{Failure, refuse, replay_sweep};
use crate::sim::Rng;

type TestResult = Result<(), Failure>;

/// Seeds per scenario: parsing is pure, so the sweep is wide.
const SEEDS: u64 = 1024;

/// One package the seed drew.
struct Drawn {
    name: String,
    version: String,
    dependencies: Vec<String>,
}

/// One seeded byte of `alphabet`.
fn pick(rng: &mut Rng, alphabet: &[u8]) -> Result<char, Failure> {
    let bound = u32::try_from(alphabet.len())?;
    let index = usize::try_from(rng.below(bound))?;
    match alphabet.get(index) {
        Some(&byte) => Ok(char::from(byte)),
        None => refuse("a draw fell outside its alphabet"),
    }
}

/// A seeded package name in Cargo's alphabet, starting with a letter.
fn draw_name(rng: &mut Rng) -> Result<String, Failure> {
    let mut name = String::from(pick(rng, b"abcdefghijklmnopqrstuvwxyz")?);
    for _ in 0..rng.below(10) {
        name.push(pick(rng, b"abcdefghijklmnopqrstuvwxyz0123456789_-")?);
    }
    Ok(name)
}

/// A seeded version `x.y.z`.
fn draw_version(rng: &mut Rng) -> String {
    format!("{}.{}.{}", rng.below(20), rng.below(20), rng.below(20))
}

/// One `dependencies` entry in a form Cargo writes: the name alone, with its
/// version, or with its version and its source in parentheses.
fn draw_entry(rng: &mut Rng) -> Result<String, Failure> {
    let name = draw_name(rng)?;
    let version = draw_version(rng);
    Ok(match rng.below(4) {
        0 => name,
        1 => format!("{name} {version}"),
        2 => format!("{name} {version} (registry+https://github.com/rust-lang/crates.io-index)"),
        _ => format!("{name} {version} (git+https://example.invalid/repo?rev=abc123#abc123def456)"),
    })
}

/// `entry` quoted as a TOML basic or literal string. An entry holds neither
/// quote character nor a backslash, so both spellings mean the same string.
fn quote(rng: &mut Rng, entry: &str) -> String {
    if rng.below(3) == 0 {
        format!("'{entry}'")
    } else {
        format!("\"{entry}\"")
    }
}

/// The `dependencies` lines for `entries`, in a seeded spelling.
fn spell_dependencies(rng: &mut Rng, entries: &[String]) -> Result<Vec<String>, Failure> {
    let key = if rng.below(5) == 0 {
        "\"dependencies\""
    } else {
        "dependencies"
    };
    let mut quoted = Vec::with_capacity(entries.len());
    for entry in entries {
        quoted.push(quote(rng, entry));
    }
    if quoted.is_empty() {
        return Ok(match rng.below(4) {
            0 => Vec::new(),
            1 => vec![format!("{key} = []")],
            2 => vec![format!("{key} = [ ]  # none")],
            _ => vec![format!("{key} = ["), "]".to_owned()],
        });
    }
    Ok(match rng.below(5) {
        // Cargo's own: one entry per line, a trailing comma.
        0 => {
            let mut lines = vec![format!("{key} = [")];
            lines.extend(quoted.iter().map(|entry| format!(" {entry},")));
            lines.push("]".to_owned());
            lines
        }
        // On one line, spaced.
        1 => vec![format!("{key} = [{}]", quoted.join(", "))],
        // On one line, compact, with a trailing comma and a comment.
        2 => vec![format!("{key}=[{},] # edited", quoted.join(","))],
        // Comments and blank lines between entries, several entries to a line.
        3 => {
            let mut lines = vec![format!("{key} = [ # the edges")];
            for pair in quoted.chunks(2) {
                lines.push(String::new());
                lines.push("  # next".to_owned());
                lines.push(format!("  {},", pair.join(", ")));
            }
            lines.push("] # end".to_owned());
            lines
        }
        // The first entry beside the bracket, the rest below, the bracket last.
        _ => {
            let mut lines = Vec::new();
            for (position, entry) in quoted.iter().enumerate() {
                if position == 0 {
                    lines.push(format!("{key} = [{entry},"));
                } else {
                    lines.push(format!("  {entry},"));
                }
            }
            lines.push("  ]".to_owned());
            lines
        }
    })
}

/// The lock text for `packages`, each array in a seeded spelling and placed
/// before or after the block's other keys.
fn spell_lock(rng: &mut Rng, packages: &[Drawn]) -> Result<String, Failure> {
    let mut lines = vec![
        "# This file is automatically @generated by Cargo.".to_owned(),
        "version = 4".to_owned(),
    ];
    for package in packages {
        lines.push(String::new());
        lines.push("[[package]]".to_owned());
        let array = spell_dependencies(rng, &package.dependencies)?;
        let first = rng.below(3) == 0;
        if first {
            lines.extend(array.iter().cloned());
        }
        lines.push(format!("name = \"{}\"", package.name));
        lines.push(format!("version = \"{}\"", package.version));
        if !first {
            lines.extend(array);
        }
    }
    lines.push(String::new());
    Ok(lines.join("\n"))
}

/// A seeded lock of up to eight packages.
fn draw_packages(rng: &mut Rng) -> Result<Vec<Drawn>, Failure> {
    let mut packages = Vec::new();
    for _ in 0..rng.between(1, 8) {
        let mut dependencies = Vec::new();
        for _ in 0..rng.below(6) {
            dependencies.push(draw_entry(rng)?);
        }
        packages.push(Drawn {
            name: draw_name(rng)?,
            version: draw_version(rng),
            dependencies,
        });
    }
    Ok(packages)
}

#[test]
fn every_spelling_of_the_array_reads_back_the_drawn_entries() -> TestResult {
    replay_sweep("lock-dependencies", SEEDS, |rng, trace| {
        let drawn = draw_packages(rng)?;
        let text = spell_lock(rng, &drawn)?;
        let read = lock::parse(&text).map_err(|error| format!("{error}\n{text}"))?;
        if read.len() != drawn.len() {
            return refuse(format!(
                "{} packages read, {} drawn\n{text}",
                read.len(),
                drawn.len()
            ));
        }
        for (expected, got) in drawn.iter().zip(&read) {
            if got.name() != expected.name
                || got.version() != expected.version
                || got.dependencies() != expected.dependencies
            {
                return refuse(format!(
                    "{} {} read as {} {} with {:?}, drawn {:?}\n{text}",
                    expected.name,
                    expected.version,
                    got.name(),
                    got.version(),
                    got.dependencies(),
                    expected.dependencies
                ));
            }
            trace.record_number("edges", got.dependencies().len());
        }
        trace.record_number("packages", read.len());
        Ok(())
    })
}

#[test]
fn a_hash_inside_a_quoted_entry_is_part_of_the_entry() -> TestResult {
    let text = "[[package]]\nname = \"app\"\nversion = \"0.1.0\"\ndependencies = [\n \"dep 1.0.0 (git+https://example.invalid/r?rev=abc#abc)\", # why\n]\n";
    let read = lock::parse(text)?;
    let kept: Vec<&str> = read
        .iter()
        .flat_map(|package| package.dependencies().iter().map(String::as_str))
        .collect();
    if kept == ["dep 1.0.0 (git+https://example.invalid/r?rev=abc#abc)"] {
        Ok(())
    } else {
        refuse(format!("read {kept:?}"))
    }
}

#[test]
fn a_dependencies_array_outside_a_package_block_is_not_a_package_edge() -> TestResult {
    let text = "[[package]]\nname = \"app\"\nversion = \"0.1.0\"\n\n[metadata]\ndependencies = [\"stray\"]\n";
    let read = lock::parse(text)?;
    let edges: usize = read
        .iter()
        .map(|package| package.dependencies().len())
        .sum();
    if edges == 0 {
        Ok(())
    } else {
        refuse(format!("{edges} edges read from a [metadata] table"))
    }
}

#[test]
fn an_array_that_is_not_a_closed_list_of_strings_is_refused_at_its_line() -> TestResult {
    let block = "[[package]]\nname = \"a\"\nversion = \"1.0.0\"\n";
    let cases: [(&str, &str, LockError); 7] = [
        (
            "an unquoted entry",
            "dependencies = [\n b,\n]\n",
            LockError::UnreadableDependencies { line: 5 },
        ),
        (
            "text after the bracket",
            "dependencies = [\"b\"] x\n",
            LockError::UnreadableDependencies { line: 4 },
        ),
        (
            "an unterminated string",
            "dependencies = [\"b\n",
            LockError::UnreadableDependencies { line: 4 },
        ),
        (
            "a number",
            "dependencies = [1]\n",
            LockError::UnreadableDependencies { line: 4 },
        ),
        (
            "a file that ends inside the array",
            "dependencies = [\n \"b\",\n",
            LockError::UnreadableDependencies { line: 4 },
        ),
        (
            "a header inside the array",
            "dependencies = [\n \"b\",\n[[package]]\n",
            LockError::UnreadableDependencies { line: 6 },
        ),
        (
            "a second array",
            "dependencies = []\ndependencies = []\n",
            LockError::DuplicateKey {
                key: "dependencies".to_owned(),
                line: 5,
            },
        ),
    ];
    for (what, array, expected) in cases {
        let text = format!("{block}{array}");
        match lock::parse(&text) {
            Err(error) if error == expected => {}
            other => return refuse(format!("{what}: expected {expected:?}, got {other:?}")),
        }
    }
    Ok(())
}
