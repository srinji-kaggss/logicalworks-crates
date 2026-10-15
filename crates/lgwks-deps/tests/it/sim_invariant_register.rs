//! The invariant register's load-time refusals, as a deterministic simulation.
//!
//! `contract/INVARIANTS.toml` is what the gate believes about which claims are
//! enforced and how. `Register::parse` answers the first of the four questions
//! the module docs separate — is the register well formed — and every refusal
//! it makes is a typed `ErrorKind` naming the entry and the defect. This family
//! draws whole registers from one seed (entries of both enforcement kinds,
//! lint and file enforcers, recorded runs, policy headers, field order), then
//! damages one seeded entry in one seeded way and requires the exact refusal
//! the module documents, naming that entry and never a neighbour.
//!
//! The evidence rule is the one with teeth: a recorded run must name the
//! entry's own enforcer. Clippy ships hundreds of lints, many sharing a prefix
//! (`panic` / `panic_in_result_fn`), so a run of one must not attest another
//! whose selector it merely contains.
//!
//! Each scenario runs twice per seed; the two receipts must agree.

use lgwks_deps::invariants::{ErrorKind, PathRefusal, Register};

use crate::replay::{Failure, refuse, replay_sweep};
use crate::sim::{Rng, Trace};

type TestResult = Result<(), Failure>;

/// Seeds per scenario.
const SEEDS: u64 = 512;

/// The fields every entry must carry, as the module's schema lists them.
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

/// The three fields a recorded run is made of, in diagnostic order.
const EVIDENCE: [&str; 3] = [
    "evidence_revision",
    "evidence_invocation",
    "evidence_result",
];

/// A real Clippy lint paired with a longer real one it is a prefix of.
const PREFIXED_LINTS: [(&str, &str); 4] = [
    ("panic", "panic_in_result_fn"),
    ("unwrap_in_result", "unwrap_in_result_fn"),
    ("indexing_slicing", "indexing_slicing_in_const"),
    ("expect_used", "expect_used_in_tests"),
];

/// Every scenario here, over the same seeds.
fn sweep(name: &str, body: impl FnMut(&mut Rng, &mut Trace) -> TestResult) -> TestResult {
    replay_sweep(name, SEEDS, body)
}

/// One seeded entry of a named constant table.
fn pick(
    rng: &mut Rng,
    table: &'static str,
    values: &[&'static str],
) -> Result<&'static str, Failure> {
    Ok(*rng.pick_named(table, values)?)
}

/// A seeded index below `len`.
fn index_below(rng: &mut Rng, len: usize) -> Result<usize, Failure> {
    Ok(usize::try_from(rng.below(u32::try_from(len)?))?)
}

/// One `[[invariant]]` block: its fields in authored order.
#[derive(Debug, Clone)]
struct Block {
    fields: Vec<(&'static str, String)>,
}

impl Block {
    /// The value written for `key`, if any.
    fn get(&self, key: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|&&(name, _)| name == key)
            .map(|pair| pair.1.as_str())
    }

    /// Replace (or append) `key`'s value.
    fn set(&mut self, key: &'static str, value: String) {
        match self.fields.iter_mut().find(|&&mut (name, _)| name == key) {
            Some(&mut (_, ref mut slot)) => *slot = value,
            None => self.fields.push((key, value)),
        }
    }

    /// Remove `key`.
    fn drop_field(&mut self, key: &str) {
        self.fields.retain(|&(name, _)| name != key);
    }

    /// The id the block writes; every drawn block writes one.
    fn id(&self) -> Result<String, Failure> {
        Ok(self
            .get("id")
            .ok_or("every drawn block writes an id")?
            .to_owned())
    }
}

/// A selector an enforcer is written as, and an invocation naming it.
fn draw_enforcer(rng: &mut Rng) -> Result<(String, String), Failure> {
    if rng.coin() {
        let namespace = pick(rng, "namespaces", &["clippy", "rustc", "rustdoc"])?;
        let name = pick(
            rng,
            "lints",
            &[
                "unwrap_used",
                "panic",
                "todo",
                "unsafe_code",
                "missing_docs",
                "broken_intra_doc_links",
            ],
        )?;
        let selector = format!("{namespace}::{name}");
        let invocation = match rng.below(3) {
            0 => format!("cargo clippy --locked -- -D {selector}"),
            1 => format!("cargo clippy --locked -- --deny={selector}"),
            _ => format!("cargo clippy --locked -- -W '{selector}' -D warnings"),
        };
        Ok((selector, invocation))
    } else {
        let krate = pick(
            rng,
            "crates",
            &["lgwks-std", "lgwks-bot", "lgwks-deps", "lgwks-ast"],
        )?;
        let file = pick(
            rng,
            "files",
            &["it/main.rs", "sim_policy.rs", "gate.rs", "it/sim_lock.rs"],
        )?;
        let selector = format!("crates/{krate}/tests/{file}");
        let invocation = if rng.coin() {
            format!("cargo nextest run --locked {selector}")
        } else {
            format!("cargo nextest run --locked -E 'test(x)' '{selector}'")
        };
        Ok((selector, invocation))
    }
}

/// A seeded valid entry numbered `number`, so ids are unique.
fn draw_block(rng: &mut Rng, number: usize) -> Result<Block, Failure> {
    let scope = pick(
        rng,
        "scopes",
        &["lgwks_std", "lgwks_bot", "lgwks_deps", "lgwks_ast"],
    )?;
    let monitor = rng.chance(300);
    let (selector, invocation) = draw_enforcer(rng)?;
    let subject = pick(
        rng,
        "subjects",
        &["The bound", "Ownership", "Order", "Isolation"],
    )?;
    let object = pick(rng, "objects", &["record", "edge", "lane", "tenant"])?;
    let enforcement = if monitor { "monitor" } else { "static-check" };
    let date = format!(
        "20{:02}-{:02}-{:02}",
        rng.between(20, 30),
        rng.between(1, 12),
        rng.between(1, 28)
    );
    let mut fields: Vec<(&'static str, String)> = vec![
        ("id", format!("INV-SIM-ENTRY{number}")),
        (
            "statement",
            format!("{subject} holds for every {object} the gate reads."),
        ),
        ("scope", scope.to_owned()),
        ("owner", scope.to_owned()),
        ("enforcement", enforcement.to_owned()),
        ("enforced_by", selector),
        ("approved_by", String::from("maintainer")),
        ("approved_on", date),
        ("review", format!("docs/review-{number}.md")),
    ];
    if monitor || rng.chance(400) {
        let digits = rng.between(7, 64);
        let revision: String = (0..digits)
            .filter_map(|_| char::from_digit(rng.below(16), 16))
            .collect();
        fields.push(("evidence_revision", revision));
        fields.push(("evidence_invocation", invocation));
        fields.push(("evidence_result", String::from("pass")));
    }
    for at in (1..fields.len()).rev() {
        let drawn = index_below(rng, at.saturating_add(1))?;
        fields.swap(at, drawn);
    }
    Ok(Block { fields })
}

/// A seeded register of 1..=16 valid entries, and its `[policy]` choice.
fn draw_register(rng: &mut Rng) -> Result<(Vec<Block>, Option<bool>), Failure> {
    let count = usize::try_from(rng.between(1, 16))?;
    let mut blocks = Vec::with_capacity(count);
    for number in 0..count {
        blocks.push(draw_block(rng, number)?);
    }
    let policy = match rng.below(3) {
        0 => None,
        1 => Some(true),
        _ => Some(false),
    };
    Ok((blocks, policy))
}

/// The rendered register and where each block's header and fields landed.
struct Layout {
    text: String,
    headers: Vec<usize>,
    field_lines: Vec<Vec<(&'static str, usize)>>,
}

impl Layout {
    /// The one-based line `key` landed on in block `block`.
    fn line_of(&self, block: usize, key: &str) -> Result<usize, Failure> {
        let placed = self.field_lines.get(block).ok_or("a layout per block")?;
        let &(_, line) = placed
            .iter()
            .find(|&&(name, _)| name == key)
            .ok_or("the key was placed")?;
        Ok(line)
    }
}

/// Render the register in the shape the shared reader reads.
fn render(blocks: &[Block], policy: Option<bool>) -> Layout {
    let mut lines = vec![String::from("# Authored invariant register (simulated).")];
    if let Some(enforce) = policy {
        lines.push(String::new());
        lines.push(String::from("[policy]"));
        lines.push(format!("enforce = {enforce}"));
    }
    let mut headers = Vec::new();
    let mut field_lines = Vec::new();
    for block in blocks {
        lines.push(String::new());
        lines.push(String::from("[[invariant]]"));
        headers.push(lines.len());
        let mut placed = Vec::new();
        for &(key, ref value) in &block.fields {
            lines.push(format!("{key} = \"{value}\""));
            placed.push((key, lines.len()));
        }
        field_lines.push(placed);
    }
    lines.push(String::new());
    Layout {
        text: lines.join("\n"),
        headers,
        field_lines,
    }
}

/// What a damage step decided: the refusal the block must now draw, or a skip
/// for a seed whose block has nothing of the kind to damage.
type Damage = Result<Option<ErrorKind>, Failure>;

/// Damage one seeded block of a seeded register and require `damage`'s refusal.
/// `damage` gets the block, its header line, and the layout's view of where
/// every field will land once rendered.
fn damaged(
    rng: &mut Rng,
    trace: &mut Trace,
    mut damage: impl FnMut(&mut Rng, &mut Block, usize) -> Damage,
) -> TestResult {
    let (mut blocks, policy) = draw_register(rng)?;
    let target = index_below(rng, blocks.len())?;
    let header = *render(&blocks, policy)
        .headers
        .get(target)
        .ok_or("a header per block")?;
    let block = blocks.get_mut(target).ok_or("the target block")?;
    let Some(expected) = damage(rng, block, header)? else {
        trace.record("skipped");
        return Ok(());
    };
    expect_refusal(&render(&blocks, policy), &expected, trace)
}

/// Parse `layout` and require exactly `expected`.
fn expect_refusal(layout: &Layout, expected: &ErrorKind, trace: &mut Trace) -> TestResult {
    match Register::parse(&layout.text) {
        Err(ref error) if error == expected => {
            trace.record(&error.to_string());
            Ok(())
        }
        other => refuse(format!(
            "answered {other:?}, expected {expected:?}\n{}",
            layout.text
        )),
    }
}

/// Every drawn register parses with one entry per block and the policy it
/// wrote (enforcing when it wrote none), and parses to one value every time.
#[test]
fn a_drawn_register_parses_with_every_entry() -> TestResult {
    sweep("valid", |rng, trace| {
        let (blocks, policy) = draw_register(rng)?;
        let layout = render(&blocks, policy);
        let register = match Register::parse(&layout.text) {
            Ok(register) => register,
            Err(error) => {
                return refuse(format!(
                    "a valid register was refused: {error}\n{}",
                    layout.text
                ));
            }
        };
        if register.entry_count() != blocks.len() {
            return refuse(format!(
                "{} blocks read as {} entries",
                blocks.len(),
                register.entry_count()
            ));
        }
        if register.enforce == matches!(policy, Some(false)) {
            return refuse(format!(
                "policy {policy:?} read as enforce = {}",
                register.enforce
            ));
        }
        if Register::parse(&layout.text).ok().as_ref() != Some(&register) {
            return refuse("the same text parsed to two registers");
        }
        trace.record_number("entries", register.entry_count());
        Ok(())
    })
}

/// A required field left out is refused at its block's header, naming the
/// entry, or no entry when the missing field is the id.
#[test]
fn a_missing_required_field_is_refused_at_its_block() -> TestResult {
    sweep("missing", |rng, trace| {
        damaged(rng, trace, |rng, block, line| {
            let field = pick(rng, "required", &REQUIRED)?;
            let id = if field == "id" {
                None
            } else {
                Some(block.id()?)
            };
            block.drop_field(field);
            Ok(Some(ErrorKind::MissingField { id, field, line }))
        })
    })
}

/// A blank free-text field is a missing field; a blank enforcement kind is a
/// value outside the closed grammar, because its repair is different.
#[test]
fn a_blank_field_is_refused_by_what_its_repair_is() -> TestResult {
    sweep("blank", |rng, trace| {
        damaged(rng, trace, |rng, block, line| {
            let field = pick(rng, "required", &REQUIRED)?;
            let blank = String::from(if rng.coin() { "" } else { "   " });
            let id = block.id()?;
            block.set(field, blank.clone());
            Ok(Some(match field {
                "enforcement" => ErrorKind::UnsupportedEnforcement {
                    id,
                    enforcement: blank,
                },
                "id" => ErrorKind::MissingField {
                    id: Some(blank),
                    field,
                    line,
                },
                _ => ErrorKind::MissingField {
                    id: Some(id),
                    field,
                    line,
                },
            }))
        })
    })
}

/// An enforcement kind outside `static-check` / `monitor` is refused with the
/// spelling it was written in.
#[test]
fn an_enforcement_outside_the_grammar_is_refused() -> TestResult {
    sweep("enforcement", |rng, trace| {
        damaged(rng, trace, |rng, block, _| {
            let kind = pick(
                rng,
                "kinds",
                &[
                    "review",
                    "intent",
                    "Static-Check",
                    "monitored",
                    "static_check",
                ],
            )?;
            block.set("enforcement", kind.to_owned());
            Ok(Some(ErrorKind::UnsupportedEnforcement {
                id: block.id()?,
                enforcement: kind.to_owned(),
            }))
        })
    })
}

/// An enforcer naming a file outside the repository is refused with why: an
/// absolute path, or a walk out with `..`.
#[test]
fn an_enforcer_outside_the_repository_is_refused() -> TestResult {
    sweep("confined", |rng, trace| {
        damaged(rng, trace, |rng, block, _| {
            let (spelling, reason) = match rng.below(3) {
                0 => ("/etc/passwd", PathRefusal::Absolute),
                1 => ("../outside/tests/a.rs", PathRefusal::ParentDirectory),
                _ => ("crates/x/../../../etc/a.rs", PathRefusal::ParentDirectory),
            };
            block.set("enforced_by", spelling.to_owned());
            // The enforcer is parsed before the evidence that must name it.
            Ok(Some(ErrorKind::EnforcerNotConfined {
                id: block.id()?,
                spelling: spelling.to_owned(),
                reason,
            }))
        })
    })
}

/// A monitor with no recorded run is refused: a run is its only proof.
#[test]
fn a_monitor_without_a_recorded_run_is_refused() -> TestResult {
    sweep("monitor", |rng, trace| {
        damaged(rng, trace, |_, block, _| {
            block.set("enforcement", String::from("monitor"));
            for field in EVIDENCE {
                block.drop_field(field);
            }
            Ok(Some(ErrorKind::MonitorNeedsEvidence { id: block.id()? }))
        })
    })
}

/// A run carrying one or two of its three fields is refused, naming the first
/// missing field in diagnostic order.
#[test]
fn a_partial_run_is_refused_naming_the_first_missing_field() -> TestResult {
    sweep("partial", |rng, trace| {
        damaged(rng, trace, |rng, block, _| {
            if block.get("evidence_revision").is_none() {
                return Ok(None);
            }
            let kept = rng.between(1, 6);
            let mut first_missing = None;
            for (bit, field) in EVIDENCE.into_iter().enumerate() {
                if kept & (1 << bit) == 0 {
                    block.drop_field(field);
                    first_missing = first_missing.or(Some(field));
                }
            }
            let missing = first_missing.ok_or("a mask below seven drops a field")?;
            Ok(Some(ErrorKind::EvidenceIncomplete {
                id: block.id()?,
                missing,
            }))
        })
    })
}

/// A run whose revision is not 7..=64 hex digits is refused with it.
#[test]
fn a_revision_not_shaped_as_one_is_refused() -> TestResult {
    sweep("revision", |rng, trace| {
        damaged(rng, trace, |rng, block, _| {
            if block.get("evidence_revision").is_none() {
                return Ok(None);
            }
            let revision = match rng.below(3) {
                0 => "a".repeat(usize::try_from(rng.between(1, 6))?),
                1 => "f".repeat(usize::try_from(rng.between(65, 80))?),
                _ => String::from("HEAD~1ab"),
            };
            block.set("evidence_revision", revision.clone());
            Ok(Some(ErrorKind::EvidenceRevisionShape {
                id: block.id()?,
                revision,
            }))
        })
    })
}

/// A run that recorded anything but `pass` is refused with what it recorded.
#[test]
fn a_run_that_did_not_pass_is_refused() -> TestResult {
    sweep("result", |rng, trace| {
        damaged(rng, trace, |rng, block, _| {
            if block.get("evidence_result").is_none() {
                return Ok(None);
            }
            let result = pick(rng, "results", &["fail", "PASS", "passed", "skipped", "ok"])?;
            block.set("evidence_result", result.to_owned());
            Ok(Some(ErrorKind::EvidenceNotPassing {
                id: block.id()?,
                result: result.to_owned(),
            }))
        })
    })
}

/// A run of some other check is refused, naming the selector it lacked.
#[test]
fn a_run_of_another_check_is_refused() -> TestResult {
    sweep("other-check", |rng, trace| {
        damaged(rng, trace, |rng, block, _| {
            let (Some(selector), Some(_)) =
                (block.get("enforced_by"), block.get("evidence_invocation"))
            else {
                return Ok(None);
            };
            let selector = selector.to_owned();
            let invocation = pick(
                rng,
                "others",
                &[
                    "cargo fmt --all --check",
                    "cargo nextest run --locked",
                    "true",
                ],
            )?;
            block.set("evidence_invocation", invocation.to_owned());
            Ok(Some(ErrorKind::EvidenceDoesNotNameEnforcer {
                id: block.id()?,
                invocation: invocation.to_owned(),
                selector,
            }))
        })
    })
}

/// A run of a lint whose name merely starts with the entry's lint, or of a
/// file whose path merely contains the entry's path, is not about the entry.
#[test]
fn a_run_of_a_check_whose_selector_contains_this_one_is_refused() -> TestResult {
    sweep("prefix", |rng, trace| {
        damaged(rng, trace, |rng, block, _| {
            let (selector, invocation) = if rng.coin() {
                let &(short, long) = PREFIXED_LINTS
                    .get(index_below(rng, PREFIXED_LINTS.len())?)
                    .ok_or("a pair")?;
                (
                    format!("clippy::{short}"),
                    format!("cargo clippy --locked -- -D clippy::{long}"),
                )
            } else {
                let file = "crates/lgwks-deps/tests/gate.rs";
                let longer = match rng.below(3) {
                    0 => format!("{file}.orig"),
                    1 => format!("vendor/{file}"),
                    _ => format!("{file}x"),
                };
                (
                    file.to_owned(),
                    format!("cargo nextest run --locked {longer}"),
                )
            };
            block.set("enforced_by", selector.clone());
            block.set("evidence_revision", String::from("0123abcd"));
            block.set("evidence_invocation", invocation.clone());
            block.set("evidence_result", String::from("pass"));
            Ok(Some(ErrorKind::EvidenceDoesNotNameEnforcer {
                id: block.id()?,
                invocation,
                selector,
            }))
        })
    })
}

/// A review date that is not a real Gregorian day is refused at its own line.
#[test]
fn an_unreal_review_date_is_refused_at_its_line() -> TestResult {
    sweep("date", |rng, trace| {
        let (mut blocks, policy) = draw_register(rng)?;
        let target = index_below(rng, blocks.len())?;
        let value = pick(
            rng,
            "dates",
            &[
                "2026-02-30",
                "2025-02-29",
                "2026-13-01",
                "2026-04-31",
                "26-10-12",
                "2026/10/12",
                "0000-01-01",
            ],
        )?;
        let block = blocks.get_mut(target).ok_or("the target block")?;
        block.set("approved_on", value.to_owned());
        let id = block.id()?;
        let layout = render(&blocks, policy);
        let line = layout.line_of(target, "approved_on")?;
        expect_refusal(
            &layout,
            &ErrorKind::BadDate {
                id,
                line,
                value: value.to_owned(),
            },
            trace,
        )
    })
}

/// A key outside the schema is refused at its own line.
#[test]
fn a_key_outside_the_schema_is_refused_at_its_line() -> TestResult {
    sweep("unknown-key", |rng, trace| {
        let (mut blocks, policy) = draw_register(rng)?;
        let target = index_below(rng, blocks.len())?;
        let key = pick(
            rng,
            "keys",
            &["enforced", "approver", "evidence", "notes", "owner_team"],
        )?;
        blocks
            .get_mut(target)
            .ok_or("the target block")?
            .fields
            .push((key, String::from("x")));
        let layout = render(&blocks, policy);
        let line = layout.line_of(target, key)?;
        expect_refusal(
            &layout,
            &ErrorKind::UnknownKey {
                line,
                key: key.to_owned(),
            },
            trace,
        )
    })
}

/// A statement no finite prefix can decide is refused at load.
#[test]
fn a_statement_no_finite_prefix_decides_is_refused() -> TestResult {
    sweep("monitorable", |rng, trace| {
        damaged(rng, trace, |rng, block, _| {
            let (statement, reason) = match rng.below(4) {
                0 => (
                    "G(request -> F answer) for every lane.",
                    "G(r -> F a) has no finite good or bad prefix",
                ),
                1 => (
                    "g ( r → f a )",
                    "G(r -> F a) has no finite good or bad prefix",
                ),
                2 => (
                    "forall(x) the edge is owned.",
                    "unbounded quantification is outside the bounded scalar domain",
                ),
                _ => (
                    "exists(y) a tenant that sees it.",
                    "unbounded quantification is outside the bounded scalar domain",
                ),
            };
            block.set("statement", statement.to_owned());
            Ok(Some(ErrorKind::NonMonitorable {
                id: block.id()?,
                reason: reason.to_owned(),
            }))
        })
    })
}
