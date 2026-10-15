//! `Cargo.lock` in every spelling TOML allows, as a deterministic simulation.
//!
//! INV-LOCK-RESOLVED-TRUTH says the gate audits the resolved graph. The reader
//! is line-oriented by design (a dependency gate must not take a TOML parser),
//! so the question this family asks is whether the lines it reads mean what
//! Cargo's own TOML reader takes them to mean. Cargo writes one canonical
//! spelling, but a lockfile is a file anyone can edit, and TOML (v1.0.0,
//! `toml.abnf`) allows more: whitespace inside `[[ package ]]`, a comment
//! after a header or a value, single-quoted literal strings, quoted keys, CRLF
//! line endings, keys in any order. A spelling the reader skips is a package
//! the audit never sees; a `source` it skips is a registry crate read as
//! local. Both fail open.
//!
//! One seed draws the whole lockfile — the packages, their kinds (workspace,
//! registry with a checksum, git), and the spelling of every header, key,
//! value and blank line — and the reader must return exactly the drawn model,
//! in order. Every scenario runs twice per seed and the two traces must hash
//! alike. Refusals are checked at the line the reader is documented to name.

use lgwks_deps::lock::{self, LockError};

use crate::sim::{Failure, Rng, Trace, refuse, replay_sweep};

type TestResult = Result<(), Failure>;

/// Seeds per scenario: parsing is pure, so the sweep is wide.
const SEEDS: u64 = 1024;

/// Every scenario here, over the same seeds.
fn sweep(name: &str, body: impl FnMut(&mut Rng, &mut Trace) -> TestResult) -> TestResult {
    replay_sweep(name, SEEDS, body)
}

/// Where a package came from, as `Cargo.lock` encodes it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Kind {
    /// No `source` key: a workspace member or a path dependency.
    Local,
    /// A registry source with the `.crate` checksum.
    Registry(String),
    /// A git source, tracked by revision, with no checksum.
    Git(String),
}

/// One package the seed drew.
#[derive(Debug, Clone)]
struct Package {
    name: String,
    version: String,
    kind: Kind,
}

/// One package as the audit reads it: name, version, local, checksum.
type Row = (String, String, bool, Option<String>);

impl Package {
    /// What the reader must return for this package.
    fn row(&self) -> Row {
        let (local, checksum) = match self.kind {
            Kind::Local => (true, None),
            Kind::Registry(ref sum) => (false, Some(sum.clone())),
            Kind::Git(_) => (false, None),
        };
        (self.name.clone(), self.version.clone(), local, checksum)
    }
}

/// Which spellings a lockfile's blocks may take.
#[derive(Debug, Clone, Copy)]
struct Spellings {
    /// Whitespace or quotes inside the header brackets.
    header_space: bool,
    /// A comment after the header.
    header_comment: bool,
    /// Single-quoted literal values.
    literal_values: bool,
    /// A comment after a value.
    value_comment: bool,
    /// Quoted keys.
    quoted_keys: bool,
    /// Blank lines, comment lines, unknown keys and dependency arrays.
    noise: bool,
    /// Keys in a seeded order.
    shuffled: bool,
}

impl Spellings {
    /// Cargo's own spelling, with nothing else.
    const CANONICAL: Self = Self {
        header_space: false,
        header_comment: false,
        literal_values: false,
        value_comment: false,
        quoted_keys: false,
        noise: false,
        shuffled: false,
    };

    /// Every spelling TOML allows.
    const ALL: Self = Self {
        header_space: true,
        header_comment: true,
        literal_values: true,
        value_comment: true,
        quoted_keys: true,
        noise: true,
        shuffled: true,
    };
}

/// One seeded byte of `alphabet`, or nothing when the draw falls outside it.
fn pick(rng: &mut Rng, alphabet: &[u8]) -> Option<char> {
    let bound = u32::try_from(alphabet.len()).ok()?;
    let index = usize::try_from(rng.below(bound)).ok()?;
    alphabet.get(index).map(|&byte| char::from(byte))
}

/// A seeded identifier in Cargo's package-name alphabet, starting with a
/// letter.
fn draw_name(rng: &mut Rng) -> String {
    let mut name: String = pick(rng, b"abcdefghijklmnopqrstuvwxyz")
        .into_iter()
        .collect();
    for _ in 0..rng.below(12) {
        name.extend(pick(rng, b"abcdefghijklmnopqrstuvwxyz0123456789_-"));
    }
    name
}

/// `count` seeded lowercase hex digits.
fn draw_hex(rng: &mut Rng, count: u32) -> String {
    (0..count)
        .filter_map(|_| pick(rng, b"0123456789abcdef"))
        .collect()
}

/// A seeded lockfile model: up to 24 packages of every kind.
fn draw_packages(rng: &mut Rng) -> Vec<Package> {
    let mut packages = Vec::new();
    for _ in 0..rng.between(0, 24) {
        let mut version = format!("{}.{}.{}", rng.below(4), rng.below(40), rng.below(12));
        if rng.chance(150) {
            version = format!("{version}-alpha.{}", rng.below(5));
        }
        let kind = match rng.below(3) {
            0 => Kind::Local,
            1 => Kind::Registry(draw_hex(rng, 64)),
            _ => Kind::Git(format!(
                "git+https://github.com/o/r?rev=v{}#{}",
                rng.below(9),
                draw_hex(rng, 40)
            )),
        };
        packages.push(Package {
            name: draw_name(rng),
            version,
            kind,
        });
    }
    packages
}

/// A key as one spelling writes it.
fn key(rng: &mut Rng, spellings: Spellings, name: &str) -> String {
    if !spellings.quoted_keys {
        return name.to_owned();
    }
    match rng.below(3) {
        0 => name.to_owned(),
        1 => format!("\"{name}\""),
        _ => format!("'{name}'"),
    }
}

/// One `key = value` line in a seeded spelling.
fn assignment(rng: &mut Rng, spellings: Spellings, name: &str, value: &str) -> String {
    let key = key(rng, spellings, name);
    let quoted = if spellings.literal_values && rng.chance(500) {
        format!("'{value}'")
    } else {
        format!("\"{value}\"")
    };
    let spacing = if spellings.noise {
        match rng.below(4) {
            0 => format!("{key}={quoted}"),
            1 => format!("{key}  =  {quoted}"),
            2 => format!("\t{key} = {quoted}"),
            _ => format!("{key} = {quoted}"),
        }
    } else {
        format!("{key} = {quoted}")
    };
    if spellings.value_comment && rng.chance(500) {
        format!("{spacing} # pinned = \"by hand\"")
    } else {
        spacing
    }
}

/// A `[[package]]` header in a seeded spelling.
fn header(rng: &mut Rng, spellings: Spellings) -> String {
    let mut line = if spellings.header_space {
        match rng.below(5) {
            0 => String::from("[[ package ]]"),
            1 => String::from("[[package ]]"),
            2 => String::from("[[\tpackage]]"),
            3 => String::from("[[\"package\"]]"),
            _ => String::from("[[package]]"),
        }
    } else {
        String::from("[[package]]")
    };
    if spellings.header_comment && rng.chance(500) {
        line.push_str(" # a [[package]] note");
    }
    line
}

/// A seeded permutation. A bound that does not fit a draw leaves the item in
/// place; no lockfile here holds four billion fields.
fn shuffle<T>(rng: &mut Rng, items: &mut [T]) {
    for index in (1..items.len()).rev() {
        let Ok(bound) = u32::try_from(index.saturating_add(1)) else {
            continue;
        };
        if let Ok(drawn) = usize::try_from(rng.below(bound)) {
            items.swap(index, drawn);
        }
    }
}

/// The lockfile text, and the one-based line of each package's header.
fn render(rng: &mut Rng, packages: &[Package], spellings: Spellings) -> (String, Vec<usize>) {
    let mut lines = vec![
        String::from("# This file is automatically @generated by Cargo."),
        String::from("# It is not intended for manual editing."),
        String::from("version = 4"),
    ];
    let mut headers = Vec::new();
    for package in packages {
        lines.push(String::new());
        lines.push(header(rng, spellings));
        headers.push(lines.len());
        let mut fields = vec![
            assignment(rng, spellings, "name", &package.name),
            assignment(rng, spellings, "version", &package.version),
        ];
        match package.kind {
            Kind::Local => {}
            Kind::Registry(ref sum) => {
                fields.push(assignment(
                    rng,
                    spellings,
                    "source",
                    "registry+https://github.com/rust-lang/crates.io-index",
                ));
                fields.push(assignment(rng, spellings, "checksum", sum));
            }
            Kind::Git(ref source) => fields.push(assignment(rng, spellings, "source", source)),
        }
        if spellings.noise && rng.chance(400) {
            fields.push(String::from(
                "dependencies = [\n \"serde\",\n \"lgwks_std 0.1.0\",\n]",
            ));
        }
        if spellings.noise && rng.chance(200) {
            fields.push(String::from("# a note inside the block"));
        }
        if spellings.shuffled {
            shuffle(rng, &mut fields);
        }
        for field in fields {
            for line in field.split('\n') {
                lines.push(line.to_owned());
            }
        }
        if spellings.noise && rng.chance(200) {
            lines.push(String::new());
        }
    }
    if spellings.noise && rng.chance(300) {
        lines.push(String::new());
        lines.push(String::from("[metadata]"));
        lines.push(String::from(
            "\"checksum foo 0.1.0 (registry+https://x)\" = \"abc\"",
        ));
    }
    lines.push(String::new());
    let ending = if spellings.noise && rng.chance(500) {
        "\r\n"
    } else {
        "\n"
    };
    (lines.join(ending), headers)
}

/// Parse `text` and require exactly `packages`, in order.
fn reads_back(text: &str, packages: &[Package], trace: &mut Trace) -> TestResult {
    let resolved = match lock::parse(text) {
        Ok(resolved) => resolved,
        Err(error) => {
            return refuse(format!(
                "the reader refused a valid lockfile: {error}\n{text}"
            ));
        }
    };
    let read: Vec<Row> = resolved
        .iter()
        .map(|row| {
            (
                row.name().to_owned(),
                row.version().to_owned(),
                row.is_local(),
                row.checksum().map(str::to_owned),
            )
        })
        .collect();
    let written: Vec<Row> = packages.iter().map(Package::row).collect();
    if read != written {
        return refuse(format!("wrote {written:?}\nread {read:?}\n{text}"));
    }
    trace.record_number("packages", read.len());
    Ok(())
}

/// Draw a model and spell it with `spellings`; the reader returns the model.
fn spelled(spellings: Spellings) -> impl FnMut(&mut Rng, &mut Trace) -> TestResult {
    move |rng, trace| {
        let packages = draw_packages(rng);
        let (text, _) = render(rng, &packages, spellings);
        reads_back(&text, &packages, trace)
    }
}

/// Cargo's own spelling reads back as the model.
#[test]
fn a_canonical_lockfile_reads_back_as_its_model() -> TestResult {
    sweep("canonical", spelled(Spellings::CANONICAL))
}

/// Blank lines, comment lines, unknown keys, dependency arrays, a metadata
/// table, spacing and CRLF change nothing the audit reads.
#[test]
fn line_noise_changes_nothing_the_audit_reads() -> TestResult {
    sweep(
        "noise",
        spelled(Spellings {
            noise: true,
            ..Spellings::CANONICAL
        }),
    )
}

/// Keys in any order within a block are the same package.
#[test]
fn key_order_within_a_block_is_the_same_package() -> TestResult {
    sweep(
        "order",
        spelled(Spellings {
            shuffled: true,
            ..Spellings::CANONICAL
        }),
    )
}

/// `[[ package ]]` and its other TOML spellings open a package block.
#[test]
fn a_header_spelled_with_toml_whitespace_opens_a_package() -> TestResult {
    sweep(
        "header-space",
        spelled(Spellings {
            header_space: true,
            ..Spellings::CANONICAL
        }),
    )
}

/// A comment after the header does not hide the package.
#[test]
fn a_header_followed_by_a_comment_opens_a_package() -> TestResult {
    sweep(
        "header-comment",
        spelled(Spellings {
            header_comment: true,
            ..Spellings::CANONICAL
        }),
    )
}

/// A single-quoted literal value is read, so a registry `source` is never
/// mistaken for a local package.
#[test]
fn a_literal_quoted_value_is_read() -> TestResult {
    sweep(
        "literal",
        spelled(Spellings {
            literal_values: true,
            ..Spellings::CANONICAL
        }),
    )
}

/// A comment after a value keeps the value, even when the comment holds an
/// `=` or a quote.
#[test]
fn a_comment_after_a_value_keeps_the_value() -> TestResult {
    sweep(
        "value-comment",
        spelled(Spellings {
            value_comment: true,
            ..Spellings::CANONICAL
        }),
    )
}

/// A quoted key names the same field as the bare key.
#[test]
fn a_quoted_key_names_the_same_field() -> TestResult {
    sweep(
        "quoted-key",
        spelled(Spellings {
            quoted_keys: true,
            ..Spellings::CANONICAL
        }),
    )
}

/// Every spelling at once, block by block.
#[test]
fn every_spelling_at_once_reads_back_as_its_model() -> TestResult {
    sweep("all", spelled(Spellings::ALL))
}

/// Whether `line` assigns `field`, in any key spelling.
fn assigns(line: &str, field: &str) -> bool {
    let unquoted = line.trim().trim_start_matches(['"', '\'']);
    match unquoted.strip_prefix(field) {
        Some(rest) => rest
            .trim_start_matches(['"', '\''])
            .trim_start()
            .starts_with('='),
        None => false,
    }
}

/// Drop the `field` line from a seeded block and require the refusal the
/// reader documents, at that block's header line.
fn dropped(field: &'static str, rng: &mut Rng, trace: &mut Trace) -> TestResult {
    let mut packages = draw_packages(rng);
    if packages.is_empty() {
        packages.push(Package {
            name: draw_name(rng),
            version: String::from("1.0.0"),
            kind: Kind::Local,
        });
    }
    let (text, headers) = render(rng, &packages, Spellings::ALL);
    let target = usize::try_from(rng.below(u32::try_from(packages.len())?))?;
    let header_line = *headers.get(target).ok_or("a header line per package")?;
    let lines: Vec<&str> = text.split('\n').collect();
    let block_end = match headers.get(target.saturating_add(1)) {
        Some(&next) => next.saturating_sub(1),
        None => lines.len(),
    };
    let Some(offset) = lines
        .get(header_line..block_end)
        .and_then(|block| block.iter().position(|line| assigns(line, field)))
    else {
        return refuse(format!(
            "block {target} has no {field} line to drop\n{text}"
        ));
    };
    let drop_at = header_line.saturating_add(offset);
    let kept: Vec<&str> = lines
        .iter()
        .enumerate()
        .filter(|&(index, _)| index != drop_at)
        .map(|(_, &line)| line)
        .collect();
    let damaged = kept.join("\n");
    let expected = match field {
        "name" => LockError::NamelessPackage { line: header_line },
        _ => LockError::VersionlessPackage { line: header_line },
    };
    // Every block before the target is whole, so the first refusal is the
    // target's own.
    match lock::parse(&damaged) {
        Err(error) if error == expected => {
            trace.record_number("refused at", header_line);
            Ok(())
        }
        other => refuse(format!(
            "dropping {field} from block {target} answered {other:?}, expected {expected:?}\n{damaged}"
        )),
    }
}

/// A block with no `name` is refused at its own header line, in any spelling.
#[test]
fn a_block_without_a_name_is_refused_at_its_header() -> TestResult {
    sweep("nameless", |rng, trace| dropped("name", rng, trace))
}

/// A block with no `version` is refused at its own header line, in any
/// spelling.
#[test]
fn a_block_without_a_version_is_refused_at_its_header() -> TestResult {
    sweep("versionless", |rng, trace| dropped("version", rng, trace))
}

/// A lockfile cut at any byte reads as a refusal or as a prefix of its model:
/// never a package whose name and version the file did not finish, and only
/// the last package read may have lost the lines after its version.
#[test]
fn a_truncated_lockfile_reads_as_a_prefix_or_a_refusal() -> TestResult {
    sweep("truncated", |rng, trace| {
        let packages = draw_packages(rng);
        let (text, _) = render(rng, &packages, Spellings::CANONICAL);
        let cut = usize::try_from(rng.below(u32::try_from(text.len().saturating_add(1))?))?;
        let prefix = text
            .get(..cut)
            .ok_or("the canonical text is ASCII, so every cut is a boundary")?;
        let Ok(resolved) = lock::parse(prefix) else {
            trace.record("refused");
            return Ok(());
        };
        if resolved.len() > packages.len() {
            return refuse("a truncated lockfile read more packages than it holds");
        }
        let last = resolved.len().checked_sub(1);
        for (index, (row, package)) in resolved.iter().zip(&packages).enumerate() {
            let whole = package.row();
            let identity = row.name() == whole.0 && row.version() == whole.1;
            let complete =
                identity && row.is_local() == whole.2 && row.checksum() == whole.3.as_deref();
            if !identity || (!complete && Some(index) != last) {
                return refuse(format!(
                    "package {index} read as {row:?}, the file wrote {package:?}\n{prefix}"
                ));
            }
        }
        trace.record_number("prefix", resolved.len());
        Ok(())
    })
}
