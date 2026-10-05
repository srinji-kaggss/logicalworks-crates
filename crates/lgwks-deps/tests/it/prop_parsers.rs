//! Property tests over the gate's two parsers, with shrinking (#273).
//!
//! The register and the lockfile are what the dependency gate believes, so a
//! parser that reads a different value than was written, or settles a repeated
//! key by taking one of the two, is a gate that can be shown a different graph
//! than the build resolved. Each property renders generated values to text,
//! parses them back, and compares; each duplicate-key property inserts a second
//! assignment and requires a refusal.

#[path = "../../../lgwks-std/tests/support/prop.rs"]
mod prop;

use lgwks_deps::contract::{Contract, ContractError};
use lgwks_deps::lock::{self, LockError};
use prop::{Outcome, check};
use proptest::collection::vec;
use proptest::prelude::{Strategy, any};
use proptest::test_runner::{TestCaseError, TestRunner};

/// The seed every run starts from. Changing it is a new corpus, not a retry.
const SEED: u64 = 0x2730_c0de_c5ee_d003;

/// A runner for this target's properties.
fn runner() -> TestRunner {
    prop::runner(SEED, 256, file!())
}

// ── the register ────────────────────────────────────────────────────────────

/// One approval, as the generator chooses it.
#[derive(Debug, Clone)]
struct Approval {
    /// Package name; unique within a register.
    krate: String,
    /// `^major.minor`.
    version: (u8, u8),
    /// The owning surface, also its one allowed consumer.
    owner: &'static str,
    /// An SPDX expression.
    license: &'static str,
    /// A non-empty subset of `normal`, `build`, `dev`.
    kinds: Vec<&'static str>,
}

/// Approvals with distinct names, up to six to a register.
fn approvals() -> impl Strategy<Value = Vec<Approval>> {
    let approval = (
        (0u8..20, 0u8..40),
        proptest::sample::select(&["lgwks_std", "lgwks_bot", "lgwks_deps", "lgwks_ast"][..]),
        proptest::sample::select(&["MIT", "Apache-2.0", "MIT OR Apache-2.0", "BSD-3-Clause"][..]),
        proptest::sample::subsequence(&["normal", "build", "dev"][..], 1..=3),
    );
    vec(approval, 0..6).prop_map(|rows| {
        rows.into_iter()
            .enumerate()
            .map(|(index, (version, owner, license, kinds))| Approval {
                krate: format!("crate-{index}"),
                version,
                owner,
                license,
                kinds,
            })
            .collect()
    })
}

/// The text a reviewer would commit for `approvals`.
fn render_register(approvals: &[Approval]) -> String {
    let mut text = String::from(
        "[policy]\nschema = 2\nenforce = true\nrepository = \"https://github.com/srinji-kaggss/logicalworks-crates\"\n",
    );
    for approval in approvals {
        text.push_str(&render_approval(approval));
    }
    text
}

/// One `[[approved]]` block.
fn render_approval(approval: &Approval) -> String {
    let (major, minor) = approval.version;
    let kinds = approval.kinds.join(",");
    [
        String::from("\n[[approved]]"),
        format!("crate = {:?}", approval.krate),
        String::from("tier = \"boundary\""),
        format!("version = \"^{major}.{minor}\""),
        format!("license = {:?}", approval.license),
        format!("owner = {:?}", approval.owner),
        format!("capability = \"test.{}\"", approval.krate),
        String::from("source = \"registry\""),
        format!("allowed_consumers = {:?}", approval.owner),
        format!("allowed_kinds = {kinds:?}"),
        String::from(
            "reason = \"A generated approval exercises the register parser under property tests.\"",
        ),
        String::from("approved_by = \"maintainer\""),
        String::from("approved_on = \"2026-10-04\""),
        String::from(
            "review = \"https://github.com/srinji-kaggss/logicalworks-crates/issues/273\"",
        ),
        String::new(),
    ]
    .join("\n")
}

/// A register renderer's signature.
type RenderRegister = fn(&[Approval]) -> String;

/// What is written is what is read: every approval parses back with its own
/// name, owner and license, and the register holds no others.
fn register_property(render: RenderRegister, approvals: &[Approval]) -> Result<(), TestCaseError> {
    let text = render(approvals);
    let contract =
        Contract::parse(&text).map_err(|error| TestCaseError::fail(format!("{error}\n{text}")))?;
    check(contract.entry_count() == approvals.len(), || {
        format!(
            "{} approvals read from {}",
            contract.entry_count(),
            approvals.len()
        )
    })?;
    for approval in approvals {
        let entry = contract.approval_for(&approval.krate);
        check(
            entry.is_some_and(|found| {
                found.owner() == approval.owner && found.license() == approval.license
            }),
            || format!("{approval:?} read back as {entry:?}"),
        )?;
    }
    Ok(())
}

#[test]
fn a_rendered_register_parses_back_to_what_was_written() -> Outcome {
    runner().run(&approvals(), |approvals| {
        register_property(render_register, &approvals)
    })?;
    Ok(())
}

#[test]
fn a_repeated_register_key_is_refused_never_settled() -> Outcome {
    let cases = (approvals(), any::<usize>(), any::<usize>());
    runner().run(&cases, |(approvals, which, line)| {
        let Some(approval) = which
            .checked_rem(approvals.len())
            .and_then(|at| approvals.get(at))
        else {
            return Ok(());
        };
        // Repeat one of the block's own assignment lines inside the block.
        let block = render_approval(approval);
        let assignments: Vec<&str> = block.lines().filter(|text| text.contains(" = ")).collect();
        let Some(&repeated) = line
            .checked_rem(assignments.len())
            .and_then(|at| assignments.get(at))
        else {
            return Ok(());
        };
        let doubled = block.replacen("\n[[approved]]", &format!("\n[[approved]]\n{repeated}"), 1);
        let text = render_register(&approvals).replacen(&block, &doubled, 1);
        let parsed = Contract::parse(&text);
        check(
            matches!(parsed, Err(ContractError::DuplicateEntryKey { .. })),
            || format!("{repeated:?} twice was answered {parsed:?}"),
        )
    })?;
    Ok(())
}

#[test]
fn the_register_property_catches_a_renderer_that_drops_the_license() -> Outcome {
    /// Writes every approval without its `license` line.
    fn unlicensed(approvals: &[Approval]) -> String {
        render_register(approvals)
            .lines()
            .filter(|text| !text.starts_with("license = "))
            .collect::<Vec<_>>()
            .join("\n")
    }
    let minimal = prop::shrunk(SEED, &approvals(), |approvals| {
        register_property(unlicensed, &approvals)
    })?;
    assert_eq!(
        minimal.len(),
        1,
        "one approval is the smallest register that shows it"
    );
    Ok(())
}

// ── the lockfile ────────────────────────────────────────────────────────────

/// One locked package, as the generator chooses it.
#[derive(Debug, Clone)]
struct Package {
    /// Package name.
    name: String,
    /// Resolved version.
    version: String,
    /// The checksum, for a registry package; `None` for a local one.
    checksum: Option<String>,
    /// A dependency row, which the reader must not mistake for a field.
    dependency: bool,
}

/// Up to eight packages, registry and local mixed.
fn packages() -> impl Strategy<Value = Vec<Package>> {
    let name = vec(
        proptest::sample::select(&['a', 'b', 'z', '0', '_', '-'][..]),
        0..8,
    )
    .prop_map(|rest| format!("p{}", rest.into_iter().collect::<String>()));
    let checksum = proptest::option::of(
        vec(proptest::sample::select(&['0', '9', 'a', 'f'][..]), 8)
            .prop_map(|digits| digits.into_iter().collect::<String>()),
    );
    let package = (name, (0u8..9, 0u8..30, 0u8..9), checksum, any::<bool>()).prop_map(
        |(name, (major, minor, patch), checksum, dependency)| Package {
            name,
            version: format!("{major}.{minor}.{patch}"),
            checksum,
            dependency,
        },
    );
    vec(package, 0..8)
}

/// The text Cargo would write for `packages`.
fn render_lock(packages: &[Package]) -> String {
    let mut lines = vec![
        String::from("# This file is automatically @generated by Cargo."),
        String::from("version = 4"),
    ];
    for package in packages {
        lines.push(String::new());
        lines.push(String::from("[[package]]"));
        lines.push(format!("name = {:?}", package.name));
        lines.push(format!("version = {:?}", package.version));
        if let Some(ref checksum) = package.checksum {
            lines.push(String::from(
                "source = \"registry+https://github.com/rust-lang/crates.io-index\"",
            ));
            lines.push(format!("checksum = {checksum:?}"));
        }
        if package.dependency {
            lines.push(String::from("dependencies = ["));
            lines.push(String::from(" \"serde\","));
            lines.push(String::from("]"));
        }
    }
    lines.push(String::new());
    lines.join("\n")
}

/// A lock reader's signature.
type ReadLock = fn(&str) -> Result<Vec<lock::Resolved>, LockError>;

/// One package as the audit reads it: name, version, local, checksum.
type Row<'a> = (&'a str, &'a str, bool, Option<&'a str>);

/// What is written is what is read, in order, field by field.
fn lock_property(read: ReadLock, packages: &[Package]) -> Result<(), TestCaseError> {
    let text = render_lock(packages);
    let resolved = read(&text).map_err(|error| TestCaseError::fail(format!("{error}\n{text}")))?;
    let read_back: Vec<Row<'_>> = resolved
        .iter()
        .map(|row| {
            (
                row.name.as_str(),
                row.version.as_str(),
                row.local,
                row.checksum.as_deref(),
            )
        })
        .collect();
    let written: Vec<Row<'_>> = packages
        .iter()
        .map(|package| {
            (
                package.name.as_str(),
                package.version.as_str(),
                package.checksum.is_none(),
                package.checksum.as_deref(),
            )
        })
        .collect();
    check(read_back == written, || {
        format!("wrote {written:?}, read {read_back:?}")
    })
}

#[test]
fn a_rendered_lockfile_parses_back_to_what_was_written() -> Outcome {
    runner().run(&packages(), |packages| {
        lock_property(lock::parse, &packages)
    })?;
    Ok(())
}

/// A second assignment of `key` in the chosen block is refused at its own
/// line, and a first assignment of a key the block lacked is accepted.
fn lock_duplicate_property(
    read: ReadLock,
    packages: &[Package],
    which: usize,
    key: &str,
) -> Result<(), TestCaseError> {
    let Some((index, target)) = which
        .checked_rem(packages.len())
        .and_then(|at| packages.get(at).map(|package| (at, package)))
    else {
        return Ok(());
    };
    // The target's own block, found by position: packages may share a name.
    let header = format!("\n[[package]]\nname = {:?}\n", target.name);
    let inserted = format!("{key} = \"second\"\n");
    let rendered = render_lock(packages);
    let Some((at, _)) = rendered.match_indices("\n[[package]]\n").nth(index) else {
        lgwks_std::trace::debug!(
            index,
            "lock_duplicate_property: the rendered lock has too few blocks"
        );
        return Err(TestCaseError::fail(format!(
            "the rendered lock has no block {index}:\n{rendered}"
        )));
    };
    let split = at.saturating_add(header.len());
    let (before, after) = rendered.split_at(split);
    let text = format!("{before}{inserted}{after}");
    // The refusal names the second assignment, whichever that is: the inserted
    // line, unless the block already assigns `key` below it.
    let inserted_line = before.lines().count().saturating_add(1);
    let assigns = format!("{key} = ");
    let line = text
        .lines()
        .enumerate()
        .skip(inserted_line)
        .take_while(|&(_, row)| !row.is_empty())
        .find(|&(_, row)| row.starts_with(&assigns))
        .map_or(inserted_line, |(index, _)| index.saturating_add(1));
    let key_already_there = key == "name"
        || key == "version"
        || (target.checksum.is_some() && (key == "source" || key == "checksum"));
    let answer = read(&text);
    if key_already_there {
        check(
            answer
                == Err(LockError::DuplicateKey {
                    key: key.to_owned(),
                    line,
                }),
            || format!("{key} twice in {target:?} was answered {answer:?}"),
        )
    } else {
        check(answer.is_ok(), || {
            format!("a single {key} was refused: {answer:?}")
        })
    }
}

/// The keys the audit reads.
const AUDITED_KEYS: [&str; 4] = ["name", "version", "source", "checksum"];

#[test]
fn a_repeated_lock_key_is_refused_at_its_own_line() -> Outcome {
    let keys = proptest::sample::select(&AUDITED_KEYS[..]);
    runner().run(
        &(packages(), any::<usize>(), keys),
        |(packages, which, key)| lock_duplicate_property(lock::parse, &packages, which, key),
    )?;
    Ok(())
}

#[test]
fn the_lock_property_catches_a_reader_that_keeps_the_last_assignment() -> Outcome {
    /// The reader as it was before #273: a repeated key overwrote the first.
    /// Emulated by deleting each earlier assignment a later line in the same
    /// block repeats, then reading what is left.
    fn last_wins(text: &str) -> Result<Vec<lock::Resolved>, LockError> {
        let mut kept: Vec<&str> = Vec::new();
        for line in text.lines() {
            let key = line.split(" = ").next().unwrap_or_default();
            if line.contains(" = ") {
                let block_start = kept
                    .iter()
                    .rposition(|earlier| earlier.starts_with('['))
                    .unwrap_or_default();
                let mut index = kept.len();
                while index > block_start {
                    index = index.saturating_sub(1);
                    if kept
                        .get(index)
                        .and_then(|earlier| earlier.split(" = ").next())
                        == Some(key)
                    {
                        drop(kept.splice(index..=index, []));
                    }
                }
            }
            kept.push(line);
        }
        lock::parse(&kept.join("\n"))
    }
    let keys = proptest::sample::select(&AUDITED_KEYS[..]);
    let minimal = prop::shrunk(
        SEED,
        &(packages(), any::<usize>(), keys),
        |(packages, which, key)| lock_duplicate_property(last_wins, &packages, which, key),
    )?;
    assert_eq!(
        (minimal.0.len(), minimal.2),
        (1, "name"),
        "one package with its name assigned twice"
    );
    Ok(())
}
