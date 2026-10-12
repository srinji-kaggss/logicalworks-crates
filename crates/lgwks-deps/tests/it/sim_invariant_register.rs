//! Seeded deterministic replay of the invariant register's load-time refusals.
//!
//! One seed draws whole registers of valid `[[invariant]]` blocks, then plants
//! one defect in one block and asserts the exact refusal `Register::parse`
//! must return, located at the block or field the defect is in. The expected
//! refusal is derived from the module's documented rule order, never read back
//! from the parser.
//!
//! Sources: runtime-verification monitorability (a `G(r -> F a)` property has
//! no finite good or bad prefix, Bauer, Leucker and Schallhart 2011), Git's
//! seven-digit abbreviation floor and SHA-256 object names for the evidence
//! revision, and path-traversal escapes (CWE-22) for `enforced_by`.
//! Requirements proved: a valid register loads every entry; each defect is
//! refused with its own typed diagnostic at its own line; the documented order
//! holds when one block carries several defects; noise-free replay is exact.

use std::collections::BTreeMap;
use std::error::Error;

use lgwks_deps::invariants::{ErrorKind, PathRefusal, Register};

use crate::sim::{Rng, Trace, receipt};

/// The seeds every family replays.
const SEEDS: [u64; 4] = [
    0x1A7A_0000_0000_0001,
    0x1A7A_0000_0000_0002,
    0x1A7A_FFFF_0000_0003,
    0xB10C_1A7A_0000_0004,
];

/// How many registers each family draws per seed.
const REGISTERS: u32 = 24;

/// The fields every entry must carry, in the order the loader checks them.
const REQUIRED: [&str; 8] = [
    "id",
    "statement",
    "scope",
    "owner",
    "enforcement",
    "approved_by",
    "approved_on",
    "review",
];

/// The fields of a recorded run, in the order the loader names a missing one.
const EVIDENCE: [&str; 3] = [
    "evidence_revision",
    "evidence_invocation",
    "evidence_result",
];

/// Statements with no temporal or quantified operator in them.
const STATEMENTS: [&str; 4] = [
    "Every authored external dependency edge names an admitted owner.",
    "A refused write leaves the destination buffer unchanged.",
    "The bot exposes exactly four fixed verbs.",
    "One parser reads the script language.",
];

/// Workspace crates an entry is scoped to and owned by.
const CRATES: [&str; 4] = ["lgwks_deps", "lgwks_bot", "lgwks_std", "lgwks_ast"];

/// Enforcers an entry may name: lint selectors and confined paths.
const ENFORCERS: [&str; 6] = [
    "clippy::unwrap_used",
    "rustc::unsafe_code",
    "rustdoc::broken_intra_doc_links",
    "crates/lgwks-deps/src/tests.rs",
    "crates/lgwks-bot/src/verb.rs",
    "scripts/gate-lanes.toml",
];

/// The hex alphabet an evidence revision is drawn from.
const HEX: [char; 16] = [
    '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'a', 'b', 'c', 'd', 'e', 'f',
];

/// A shared test result.
type TestResult = Result<(), Box<dyn Error>>;

/// One drawn `[[invariant]]` block: its identifier and its fields in order.
#[derive(Debug, Clone)]
struct Block {
    /// The `id` the block was drawn with.
    id: String,
    /// Every `key = "value"` line, in render order.
    fields: Vec<(&'static str, String)>,
}

impl Block {
    /// The value of `key`, when the block writes it.
    fn get(&self, key: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|entry| entry.0 == key)
            .map(|entry| entry.1.as_str())
    }

    /// Sets `key` to `value`, appending it when absent.
    fn set(&mut self, key: &'static str, value: String) {
        match self.fields.iter_mut().find(|entry| entry.0 == key) {
            Some(entry) => entry.1 = value,
            None => self.fields.push((key, value)),
        }
    }

    /// Removes `key`.
    fn omit(&mut self, key: &str) {
        self.fields.retain(|entry| entry.0 != key);
    }
}

/// A rendered register and where each block and field landed, one-based.
struct Rendered {
    /// The register text.
    text: String,
    /// Each block's header line.
    headers: Vec<usize>,
    /// Each block's field lines.
    lines: Vec<BTreeMap<&'static str, usize>>,
}

impl Rendered {
    /// The header line of `block`.
    fn header(&self, block: usize) -> Result<usize, Box<dyn Error>> {
        Ok(self
            .headers
            .get(block)
            .copied()
            .ok_or("no header for that block")?)
    }

    /// The line `key` was written on in `block`.
    fn line(&self, block: usize, key: &str) -> Result<usize, Box<dyn Error>> {
        Ok(self
            .lines
            .get(block)
            .and_then(|lines| lines.get(key))
            .copied()
            .ok_or("that field was not written")?)
    }
}

/// `count` hex digits.
fn hex(rng: &mut Rng, count: u32) -> Result<String, Box<dyn Error>> {
    (0..count)
        .map(|_| rng.pick_named("hex", &HEX).copied())
        .collect::<Result<String, _>>()
        .map_err(Into::into)
}

/// Adds a complete, passing recorded run naming the block's enforcer.
fn add_evidence(rng: &mut Rng, block: &mut Block) -> TestResult {
    let length = rng.between(7, 64);
    block.set("evidence_revision", hex(rng, length)?);
    let invocation = match block.get("enforced_by") {
        Some(selector) => format!("cargo nextest run --locked -E {selector}"),
        None => "cargo nextest run --locked".to_owned(),
    };
    block.set("evidence_invocation", invocation);
    block.set("evidence_result", "pass".to_owned());
    Ok(())
}

/// One valid block, numbered `index`.
fn draw_block(rng: &mut Rng, index: u32) -> Result<Block, Box<dyn Error>> {
    let monitor = rng.coin();
    let mut block = Block {
        id: format!("INV-SIM-{index}"),
        fields: Vec::new(),
    };
    block.set("id", block.id.clone());
    block.set("statement", (*rng.pick_named("statements", &STATEMENTS)?).to_owned());
    block.set("scope", (*rng.pick_named("crates", &CRATES)?).to_owned());
    block.set("owner", (*rng.pick_named("crates", &CRATES)?).to_owned());
    let kind = if monitor { "monitor" } else { "static-check" };
    block.set("enforcement", kind.to_owned());
    if rng.chance(800) {
        block.set("enforced_by", (*rng.pick_named("enforcers", &ENFORCERS)?).to_owned());
    }
    block.set("approved_by", "maintainer".to_owned());
    let date = format!("2026-{:02}-{:02}", rng.between(1, 12), rng.between(1, 28));
    block.set("approved_on", date);
    block.set("review", "crates/lgwks-deps/src/lib.rs".to_owned());
    if monitor || rng.coin() {
        add_evidence(rng, &mut block)?;
    }
    Ok(block)
}

/// A register of `low..=high` valid blocks.
fn draw_register(rng: &mut Rng, low: u32, high: u32) -> Result<Vec<Block>, Box<dyn Error>> {
    (0..rng.between(low, high))
        .map(|index| draw_block(rng, index))
        .collect()
}

/// Renders `blocks` under a `[policy]` section with `enforce`.
fn render(blocks: &[Block], enforce: bool) -> Rendered {
    let mut lines: Vec<String> = vec![
        "# Authored invariant register.".to_owned(),
        String::new(),
        "[policy]".to_owned(),
        format!("enforce = {enforce}"),
    ];
    let mut headers = Vec::new();
    let mut field_lines = Vec::new();
    for block in blocks {
        lines.push(String::new());
        lines.push("[[invariant]]".to_owned());
        headers.push(lines.len());
        let mut at = BTreeMap::new();
        for &(key, ref value) in &block.fields {
            lines.push(format!("{key} = \"{value}\""));
            at.insert(key, lines.len());
        }
        field_lines.push(at);
    }
    Rendered {
        text: lines.join("\n"),
        headers,
        lines: field_lines,
    }
}

/// A block index drawn from `len` blocks.
fn pick_block(rng: &mut Rng, len: usize) -> Result<usize, Box<dyn Error>> {
    let indices: Vec<usize> = (0..len).collect();
    Ok(*rng.pick_named("blocks", &indices)?)
}

/// What a family plants: it edits the chosen block and, given where the
/// register rendered, names the refusal the loader must return.
type Plant = fn(&mut Rng, &mut Block) -> Result<Expected, Box<dyn Error>>;

/// The refusal a planted defect must produce, before line numbers are known.
type Expected = Box<dyn Fn(&Rendered, usize, &str) -> Result<ErrorKind, Box<dyn Error>>>;

/// Draws registers, plants one defect with `plant`, and asserts the refusal.
fn assert_planted(plant: Plant) -> TestResult {
    for seed in SEEDS {
        let mut rng = Rng::new(seed);
        for _ in 0..REGISTERS {
            let mut blocks = draw_register(&mut rng, 1, 6)?;
            let chosen = pick_block(&mut rng, blocks.len())?;
            let block = blocks.get_mut(chosen).ok_or("chosen block exists")?;
            let id = block.id.clone();
            let expected = plant(&mut rng, block)?;
            let rendered = render(&blocks, rng.coin());
            assert_eq!(
                Register::parse(&rendered.text),
                Err(expected(&rendered, chosen, &id)?),
                "seed {seed:#x}: block {chosen} must be refused as planted"
            );
        }
    }
    Ok(())
}

#[test]
/// A register of valid blocks loads every entry and its `enforce` policy.
fn a_valid_register_loads_every_entry() -> TestResult {
    for seed in SEEDS {
        let mut rng = Rng::new(seed);
        for _ in 0..REGISTERS {
            let blocks = draw_register(&mut rng, 0, 10)?;
            let enforce = rng.coin();
            let register = Register::parse(&render(&blocks, enforce).text)?;
            assert_eq!(
                register.entry_count(),
                blocks.len(),
                "seed {seed:#x}: one entry per block"
            );
            assert_eq!(register.enforce, enforce, "seed {seed:#x}: the policy is read");
        }
    }
    Ok(())
}

#[test]
/// A required field left out is refused at the block's header, naming the
/// field and, unless the field is the id itself, the invariant.
fn a_missing_required_field_is_refused_at_its_block() -> TestResult {
    assert_planted(|rng, block| {
        let field = *rng.pick_named("required", &REQUIRED)?;
        block.omit(field);
        Ok(Box::new(move |rendered, chosen, id| {
            Ok(ErrorKind::MissingField {
                id: (field != "id").then(|| id.to_owned()),
                field,
                line: rendered.header(chosen)?,
            })
        }))
    })
}

#[test]
/// A blank free-text field is a missing one, refused the same way.
fn a_blank_free_text_field_is_refused_as_missing() -> TestResult {
    assert_planted(|rng, block| {
        let field = *rng.pick_named(
            "free text",
            &["statement", "scope", "owner", "approved_by", "approved_on", "review"],
        )?;
        block.set(field, "   ".to_owned());
        Ok(Box::new(move |rendered, chosen, id| {
            Ok(ErrorKind::MissingField {
                id: Some(id.to_owned()),
                field,
                line: rendered.header(chosen)?,
            })
        }))
    })
}

#[test]
/// A review date that is not a real `YYYY-MM-DD` is refused at its own line.
fn a_malformed_review_date_is_refused_at_its_line() -> TestResult {
    assert_planted(|rng, block| {
        let value = *rng.pick_named(
            "bad dates",
            &[
                "2026-13-01",
                "2026-00-10",
                "2026-01-32",
                "2026/01/01",
                "26-01-01",
                "2026-1-01",
                "tomorrow",
            ],
        )?;
        block.set("approved_on", value.to_owned());
        Ok(Box::new(move |rendered, chosen, id| {
            Ok(ErrorKind::BadDate {
                id: id.to_owned(),
                line: rendered.line(chosen, "approved_on")?,
                value: value.to_owned(),
            })
        }))
    })
}

#[test]
/// A statement with an unbounded eventuality or quantifier is refused as not
/// monitorable, with the reason that class of statement has.
fn a_non_monitorable_statement_is_refused_with_its_reason() -> TestResult {
    assert_planted(|rng, block| {
        let (statement, reason) = *rng.pick_named(
            "non-monitorable",
            &[
                ("G(request -> F reply)", "G(r -> F a) has no finite good or bad prefix"),
                ("g ( a \u{2192} f b )", "G(r -> F a) has no finite good or bad prefix"),
                ("forall(x) holds", "unbounded quantification is outside the bounded scalar domain"),
                ("exists(y) fails", "unbounded quantification is outside the bounded scalar domain"),
                ("\u{2200} tenant isolated", "unbounded quantification is outside the bounded scalar domain"),
            ],
        )?;
        block.set("statement", statement.to_owned());
        Ok(Box::new(move |_, _, id| {
            Ok(ErrorKind::NonMonitorable {
                id: id.to_owned(),
                reason: reason.to_owned(),
            })
        }))
    })
}

#[test]
/// An enforcement kind outside the two-word grammar is refused verbatim.
fn an_enforcement_kind_outside_the_grammar_is_refused() -> TestResult {
    assert_planted(|rng, block| {
        let kind = *rng.pick_named(
            "kinds",
            &["review", "intent", "Static-Check", "static_check", "monitored"],
        )?;
        block.set("enforcement", kind.to_owned());
        Ok(Box::new(move |_, _, id| {
            Ok(ErrorKind::UnsupportedEnforcement {
                id: id.to_owned(),
                enforcement: kind.to_owned(),
            })
        }))
    })
}

#[test]
/// An `enforced_by` that escapes the repository is refused with the escape.
fn an_enforcer_that_escapes_the_repository_is_refused() -> TestResult {
    assert_planted(|rng, block| {
        let (spelling, reason) = *rng.pick_named(
            "escapes",
            &[
                ("/etc/passwd", PathRefusal::Absolute),
                ("/abs/check.rs", PathRefusal::Absolute),
                ("../outside.rs", PathRefusal::ParentDirectory),
                ("crates/../../x.rs", PathRefusal::ParentDirectory),
                ("tests/a/../b.rs", PathRefusal::ParentDirectory),
            ],
        )?;
        block.set("enforced_by", spelling.to_owned());
        Ok(Box::new(move |_, _, id| {
            Ok(ErrorKind::EnforcerNotConfined {
                id: id.to_owned(),
                spelling: spelling.to_owned(),
                reason,
            })
        }))
    })
}

#[test]
/// A monitor with no recorded run is refused: a run is its only proof.
fn a_monitor_with_no_recorded_run_is_refused() -> TestResult {
    assert_planted(|_, block| {
        block.set("enforcement", "monitor".to_owned());
        for field in EVIDENCE {
            block.omit(field);
        }
        Ok(Box::new(|_, _, id| {
            Ok(ErrorKind::MonitorNeedsEvidence { id: id.to_owned() })
        }))
    })
}

#[test]
/// A recorded run with a field missing names the first missing one.
fn an_incomplete_run_names_its_first_missing_field() -> TestResult {
    assert_planted(|rng, block| {
        add_evidence(rng, block)?;
        let mut kept = Vec::new();
        for field in EVIDENCE {
            if rng.coin() {
                kept.push(field);
            } else {
                block.omit(field);
            }
        }
        if kept.len() == EVIDENCE.len() {
            block.omit("evidence_invocation");
            kept.retain(|field| *field != "evidence_invocation");
        }
        if kept.is_empty() {
            // No run at all is not an incomplete run; keep one field.
            add_evidence(rng, block)?;
            block.omit("evidence_result");
            kept = vec!["evidence_revision", "evidence_invocation"];
        }
        let missing = EVIDENCE
            .into_iter()
            .find(|field| !kept.contains(field))
            .ok_or("one field is missing")?;
        Ok(Box::new(move |_, _, id| {
            Ok(ErrorKind::EvidenceIncomplete {
                id: id.to_owned(),
                missing,
            })
        }))
    })
}

#[test]
/// A revision shorter than Git's abbreviation floor, longer than a SHA-256
/// name, or not hex is refused as not naming a revision.
fn a_revision_that_names_no_revision_is_refused() -> TestResult {
    assert_planted(|rng, block| {
        add_evidence(rng, block)?;
        let revision = match rng.below(3) {
            0 => hex(rng, 6)?,
            1 => hex(rng, 65)?,
            _ => "ghijklm".to_owned(),
        };
        block.set("evidence_revision", revision.clone());
        Ok(Box::new(move |_, _, id| {
            Ok(ErrorKind::EvidenceRevisionShape {
                id: id.to_owned(),
                revision: revision.clone(),
            })
        }))
    })
}

#[test]
/// Only the exact result `pass` is evidence that the check held.
fn a_result_other_than_pass_is_refused() -> TestResult {
    assert_planted(|rng, block| {
        add_evidence(rng, block)?;
        let result = *rng.pick_named("results", &["fail", "PASS", "passed", "ok"])?;
        block.set("evidence_result", result.to_owned());
        Ok(Box::new(move |_, _, id| {
            Ok(ErrorKind::EvidenceNotPassing {
                id: id.to_owned(),
                result: result.to_owned(),
            })
        }))
    })
}

#[test]
/// A run of something other than the entry's own enforcer is refused.
fn a_run_that_does_not_name_the_enforcer_is_refused() -> TestResult {
    assert_planted(|rng, block| {
        let selector = *rng.pick_named("enforcers", &ENFORCERS)?;
        block.set("enforced_by", selector.to_owned());
        add_evidence(rng, block)?;
        let invocation = "cargo build --locked".to_owned();
        block.set("evidence_invocation", invocation.clone());
        Ok(Box::new(move |_, _, id| {
            Ok(ErrorKind::EvidenceDoesNotNameEnforcer {
                id: id.to_owned(),
                invocation: invocation.clone(),
                selector: selector.to_owned(),
            })
        }))
    })
}

#[test]
/// A key outside the schema is refused at its own line, before any block is
/// built.
fn a_key_outside_the_schema_is_refused_at_its_line() -> TestResult {
    assert_planted(|rng, block| {
        let key = *rng.pick_named("unknown keys", &["severity", "priority", "notes"])?;
        block.set(key, "high".to_owned());
        Ok(Box::new(move |rendered, chosen, _| {
            Ok(ErrorKind::UnknownKey {
                line: rendered.line(chosen, key)?,
                key: key.to_owned(),
            })
        }))
    })
}

#[test]
/// A key written before any section belongs to nothing and is refused there.
fn a_key_before_any_section_is_refused() -> TestResult {
    for seed in SEEDS {
        let mut rng = Rng::new(seed);
        for _ in 0..REGISTERS {
            let blocks = draw_register(&mut rng, 0, 4)?;
            let key = *rng.pick_named("orphans", &["id", "statement", "owner"])?;
            let text = format!("{key} = \"orphan\"\n{}", render(&blocks, true).text);
            assert_eq!(
                Register::parse(&text),
                Err(ErrorKind::OrphanKey {
                    line: 1,
                    key: key.to_owned()
                }),
                "seed {seed:#x}: {key} before any section"
            );
        }
    }
    Ok(())
}

#[test]
/// With several defects in one block, the documented order decides which is
/// reported: required fields, then the date, then the enforcement kind, then
/// the recorded run.
fn several_defects_report_in_the_documented_order() -> TestResult {
    assert_planted(|rng, block| {
        add_evidence(rng, block)?;
        block.set("evidence_result", "fail".to_owned());
        block.set("enforcement", "review".to_owned());
        block.set("approved_on", "2026/01/01".to_owned());
        let drop_review = rng.coin();
        if drop_review {
            block.omit("review");
        }
        Ok(Box::new(move |rendered, chosen, id| {
            Ok(if drop_review {
                ErrorKind::MissingField {
                    id: Some(id.to_owned()),
                    field: "review",
                    line: rendered.header(chosen)?,
                }
            } else {
                ErrorKind::BadDate {
                    id: id.to_owned(),
                    line: rendered.line(chosen, "approved_on")?,
                    value: "2026/01/01".to_owned(),
                }
            })
        }))
    })
}

#[test]
/// Each line-bearing refusal names its line in its message.
fn refusals_name_their_line_in_the_message() -> TestResult {
    let mut rng = Rng::new(SEEDS[0]);
    for _ in 0..REGISTERS {
        let line = usize::try_from(rng.between(1, 9_999))?;
        assert_eq!(
            ErrorKind::OrphanKey {
                line,
                key: "id".to_owned()
            }
            .to_string(),
            format!("line {line}: invariant key \"id\" appears before any section header"),
            "the orphan refusal names line {line}"
        );
        assert_eq!(
            ErrorKind::MissingField {
                id: None,
                field: "id",
                line
            }
            .to_string(),
            format!("line {line}: invariant block is missing required field \"id\""),
            "a nameless block is named by its line {line}"
        );
    }
    Ok(())
}

/// Loads drawn registers and returns the replay receipt of what loaded.
fn register_trace(seed: u64) -> Result<u64, Box<dyn Error>> {
    let mut rng = Rng::new(seed);
    let mut trace = Trace::new();
    for _ in 0..REGISTERS {
        let blocks = draw_register(&mut rng, 0, 8)?;
        let enforce = rng.coin();
        let rendered = render(&blocks, enforce);
        let register = Register::parse(&rendered.text)?;
        trace.record_number("entries", register.entry_count());
        trace.record_number("enforce", register.enforce);
        trace.record_number("bytes", rendered.text.len());
    }
    Ok(receipt(&trace)?)
}

#[test]
/// The replay oracle: one seed, one receipt.
fn the_same_seed_replays_to_the_same_register_receipt() -> TestResult {
    for seed in SEEDS {
        assert_eq!(
            register_trace(seed)?,
            register_trace(seed)?,
            "seed {seed:#x}: the same seed must replay"
        );
    }
    Ok(())
}

#[test]
/// The divergence oracle: a sweep that ignored its seed would pass the replay.
fn distinct_register_seeds_diverge() -> TestResult {
    assert_ne!(
        register_trace(SEEDS[0])?,
        register_trace(SEEDS[1])?,
        "two seeds drew the same registers"
    );
    Ok(())
}
