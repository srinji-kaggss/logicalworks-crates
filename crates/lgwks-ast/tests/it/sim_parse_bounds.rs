//! Deterministic simulation of the bounds a hostile source can trip (#277).
//!
//! One seed writes one source shaped like an attack rather than like code: a
//! run of openers nested deeper than [`MAX_AST_DEPTH`], the same run with its
//! closers, the closers with no openers, and mixtures with real statements
//! interleaved so the grammar cannot simply reject the whole file. The shipped
//! [`try_parse`] runs on it through the public API, and every outcome is
//! required to be one of the crate's own typed refusals or a tree — never a
//! panic, never a silent answer.
//!
//! The seeds also *decide* the ceiling each seed is held to. One seed's tree is
//! refused for depth, the next for node count, the next for recovery nodes, and
//! the next is small enough to parse cleanly. That is the point: the four arms
//! are different facts, and a seed schedule that only ever produced one of them
//! would prove the refusal machinery rather than the bounds.
//!
//! # Replay
//!
//! Every seed's whole scenario — the source's shape, the refusal's own `Debug`,
//! and the metrics — is recorded into a trace and hashed, and the same seed
//! replays to the same hash. A failure prints its seed, so the exact input is
//! reachable from the message alone. Two seeds over a hundred are required to
//! produce distinct traces, which is what stops a generator that quietly stopped
//! varying from reading as a passing determinism test.
//!
//! The seed substrate (`Rng`, `Trace`) is the one the `lgwks_bot` simulation
//! families run on, included by path from this binary's root so a seed means
//! the same draw sequence in every module and in every suite.

#![cfg(feature = "lang-rust")]

use crate::seed;
use std::collections::BTreeSet;
use std::error::Error;

use lgwks_ast::{
    Language, MAX_AST_DEPTH, MAX_AST_NODES, MAX_SOURCE_BYTES, ParseError, inspect_ast, try_parse,
};
use seed::{Rng, Trace};

/// What every family returns.
type TestResult = Result<(), Box<dyn Error>>;

/// What a scenario step returns: the value it drew, or the refusal that stopped
/// it. A draw this target cannot hold is a refusal, never a substitute count,
/// because a scenario that silently measured zero would still record a trace.
type Scenario<T> = Result<T, Box<dyn Error>>;

/// Sources written per family.
const SEEDS: u64 = 96;

/// The base every family's seeds are derived from.
const BASE_SEED: u64 = 0x0da5_2770_0000_0001;

/// The four shapes a generated source can take.
///
/// Each is a different way to be hostile, and each has a different expected arm,
/// which is what makes the family a test of the bounds rather than of one
/// refusal.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Shape {
    /// Openers only: the tree is as deep as the nesting and as wide as the
    /// grammar's recovery makes it.
    UnbalancedOpeners,
    /// Openers then closers: a deep, narrow tree that parses.
    Balanced,
    /// Closers only: the mirror image, and a different tree again.
    UnbalancedClosers,
    /// Openers interleaved with valid items: a deep tree with real nodes in it,
    /// so the walk cannot refuse it for being trivial.
    Mixed,
}

impl Shape {
    /// The shape's own name, as recorded in a trace.
    fn name(self) -> &'static str {
        match self {
            Self::UnbalancedOpeners => "unbalanced-openers",
            Self::Balanced => "balanced",
            Self::UnbalancedClosers => "unbalanced-closers",
            Self::Mixed => "mixed",
        }
    }

    /// Every shape, in a fixed order, so a seed draws from a constant pool.
    const ALL: [Shape; 4] = [
        Self::UnbalancedOpeners,
        Self::Balanced,
        Self::UnbalancedClosers,
        Self::Mixed,
    ];
}

/// The seed for run `index` of `family`; families take disjoint streams.
fn seed_for(family: u64, index: u64) -> u64 {
    BASE_SEED
        ^ family.wrapping_mul(0x9e37_79b9_7f4a_7c15)
        ^ index.wrapping_mul(0x2545_f491_4f6c_dd1d)
}

/// Rust statements that parse cleanly, so a mixed shape is not rejected whole.
const CLEAN: [&str; 4] = ["let a = 1;", "let b: u32 = 2;", "return;", "let c = a + b;"];

/// One entry of `pool`, drawn from it.
///
/// The generator draws `u32` and a slice is indexed by `usize`, and `usize` is
/// sixteen bits on the smallest target Rust supports, so the conversion is
/// genuinely fallible. Every pool here holds a handful of entries, so a host
/// that cannot hold the draw is reported rather than answered with the first
/// entry — a family that silently drew `Shape::Balanced` ninety-six times would
/// still record a trace and still pass.
fn drawn<T: Copy>(pool: &[T], rng: &mut Rng) -> Scenario<T> {
    let bound = u32::try_from(pool.len())?;
    let at = usize::try_from(rng.below(bound))?;
    pool.get(at)
        .copied()
        .ok_or_else(|| format!("a draw from {} entries names none of them", pool.len()).into())
}

/// The source `rng` writes, and the shape it wrote.
fn write_source(rng: &mut Rng) -> Scenario<(String, Shape)> {
    let shape = drawn(&Shape::ALL, rng)?;
    // The nesting depth is drawn log-uniformly across the ceiling, not
    // uniformly: a uniform draw from 1 to 2048 puts fewer than one seed in 96
    // inside a depth of 16, so the family would have no accepting arm at all and
    // the smallest refusal it could reach would be a trivial one. A power of two
    // per draw puts five of twelve outcomes inside 16 and half past the depth
    // ceiling, so both arms are populated without either being a special case.
    // The ceiling of 2048 is well inside sixteen bits, so the draw fits a
    // `usize` on every target that can run this suite.
    let exponent = rng.between(0, 11);
    let levels = usize::try_from(1_u32 << exponent)?;
    let source = match shape {
        Shape::UnbalancedOpeners => format!("{}fn f() {{}}", "fn f() {".repeat(levels)),
        Shape::Balanced => format!(
            "{}fn f() {{}}{}",
            "fn f() {".repeat(levels),
            "}".repeat(levels)
        ),
        Shape::UnbalancedClosers => format!("fn f() {{}}{}", "}".repeat(levels)),
        Shape::Mixed => {
            let mut source = String::new();
            for level in 0..levels {
                source.push_str("fn f() {");
                if level % 8 == 0 {
                    source.push_str(drawn(&CLEAN, rng)?);
                }
            }
            source.push('}');
            source.push_str(&"}".repeat(levels));
            source
        }
    };
    Ok((source, shape))
}

/// The name of the arm `try_parse` answered with, or `accepted`.
fn arm(answer: &Result<lgwks_ast::Parsed, ParseError>) -> String {
    // Matched through the reference rather than by value, and the one arm that
    // binds a payload binds it by `ref`: this reads the caller's own result and
    // must not move anything out of it, because the caller still reports the
    // same refusal afterwards.
    match *answer {
        Ok(_) => "accepted".to_owned(),
        Err(ParseError::SourceTooLarge { .. }) => "source-too-large".to_owned(),
        Err(ParseError::ParserUnavailable { .. }) => "parser-unavailable".to_owned(),
        Err(ParseError::InvalidSyntax { .. }) => "invalid-syntax".to_owned(),
        Err(ParseError::AstTooLarge { .. }) => "ast-too-large".to_owned(),
        Err(ParseError::AstTooDeep { .. }) => "ast-too-deep".to_owned(),
        Err(ref other) => format!("unclassified:{other}"),
    }
}

/// One seed's whole scenario, recorded.
fn scenario(seed: u64) -> Scenario<Trace> {
    let mut rng = Rng::new(seed);
    let (source, shape) = write_source(&mut rng)?;
    let mut trace = Trace::new();
    trace.record(shape.name());
    trace.record_number("bytes", source.len());
    // No timing goes into the trace: a hash over a clock reading is a hash over
    // the machine, and a replay receipt that diverges on a loaded host has
    // stopped being a receipt. The durations live in the measurement rig, which
    // is where a number that varies between runs belongs.
    let answer = try_parse(&source, Language::Rust);
    trace.record(&arm(&answer));
    // The refusal's own rendering, so a change in what a caller would be told
    // changes the hash rather than passing silently.
    // Bound by reference so the trace records the tree's findings without
    // consuming the tree, which the caller may still want.
    match answer {
        Err(ref error) => trace.record(&error.to_diagnostic("sim.rs", &source).render()),
        Ok(ref tree) => {
            let metrics = inspect_ast(&tree.root(), None);
            trace.record_number("nodes", metrics.nodes);
            trace.record_number("depth", metrics.max_depth);
        }
    }
    Ok(trace)
}

/// Runs `family` over [`SEEDS`] generated sources, handing each to `check`.
fn sweep(
    family: u64,
    mut check: impl FnMut(u64, &str, Shape, &Result<lgwks_ast::Parsed, ParseError>) -> TestResult,
) -> TestResult {
    for index in 0..SEEDS {
        let seed = seed_for(family, index);
        let mut rng = Rng::new(seed);
        let (source, shape) = write_source(&mut rng)?;
        let answer = try_parse(&source, Language::Rust);
        check(seed, &source, shape, &answer).map_err(|error| format!("seed {seed:#x}: {error}"))?;
    }
    Ok(())
}

#[test]
/// Every generated source answers with a tree or a typed refusal.
fn every_generated_source_answers_typed() -> TestResult {
    sweep(1, |seed, source, shape, answer| {
        match *answer {
            Ok(ref tree) => {
                let metrics = inspect_ast(&tree.root(), None);
                assert!(
                    metrics.nodes <= MAX_AST_NODES,
                    "seed {seed:#x}: an accepted {} source reported {} nodes",
                    shape.name(),
                    metrics.nodes
                );
                assert!(
                    metrics.max_depth <= MAX_AST_DEPTH,
                    "seed {seed:#x}: an accepted {} source is {} deep, past the ceiling of {MAX_AST_DEPTH}",
                    shape.name(),
                    metrics.max_depth
                );
                assert!(
                    !metrics.has_syntax_issues,
                    "seed {seed:#x}: an accepted {} source carries a recovery node",
                    shape.name()
                );
            }
            Err(ref error) => {
                assert!(
                    !error.to_string().is_empty(),
                    "seed {seed:#x}: the refusal rendered as nothing"
                );
                assert!(
                    source.len() <= MAX_SOURCE_BYTES,
                    "seed {seed:#x}: a {} byte source was parsed at all, so the byte bound let it through",
                    source.len()
                );
            }
        }
        Ok(())
    })
}

/// The arm each shape is expected to reach, at the depths this generator draws.
///
/// This is the map the family is built around, and it is the measured one rather
/// than the guessed one. A source whose delimiters are all *opened* does not
/// produce a deep tree at all: tree-sitter's recovery collapses the whole run
/// into one `ERROR` node near the root, so the tree is **wide and shallow** and
/// the refusal is for syntax. A source whose delimiters are opened *and closed*
/// produces a genuinely deep tree, and the refusal is for its depth. Pinning the
/// map is what makes the two bounds distinguishable in evidence: a change in
/// upstream recovery shows up here as a changed arm, rather than as a bound that
/// quietly stopped firing.
const SHAPE_ARMS: [(&str, &str); 4] = [
    ("unbalanced-openers", "invalid-syntax"),
    ("unbalanced-closers", "invalid-syntax"),
    ("balanced", "ast-too-deep-or-accepted"),
    ("mixed", "ast-too-deep-or-invalid-syntax"),
];

#[test]
/// Each generated shape reaches the arm it is shaped to reach.
fn each_shape_reaches_its_own_arm() -> TestResult {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    sweep(2, |_seed, _source, shape, answer| {
        seen.insert(format!("{}={}", shape.name(), arm(answer)));
        Ok(())
    })?;
    for (shape, expected) in SHAPE_ARMS {
        let matching: Vec<&String> = seen
            .iter()
            .filter(|entry| entry.starts_with(&format!("{shape}=")))
            .collect();
        assert!(
            !matching.is_empty(),
            "no seed produced the `{shape}` shape; the family is not testing it"
        );
        let any_expected = matching.iter().any(|entry| {
            expected
                .split("-or-")
                .any(|one| entry.ends_with(&format!("={one}")))
        });
        assert!(
            any_expected,
            "the `{shape}` shape reached {matching:?}; this generator draws a depth across \
             the ceiling, so one of {expected} is expected"
        );
    }
    Ok(())
}

#[test]
/// A source nested past the depth ceiling is refused as `AstTooDeep`, and the
/// refusal names both the ceiling and the depth observed.
fn a_source_past_the_depth_ceiling_is_refused_for_its_depth() -> TestResult {
    let mut refused = 0_u64;
    sweep(3, |seed, _source, shape, answer| {
        // Only the shapes that actually nest deeply: an unbalanced run recovers
        // into a shallow tree, so charging it to the depth bound would be
        // charging the wrong bound for the right reason.
        if shape != Shape::Balanced && shape != Shape::Mixed {
            return Ok(());
        }
        if let Err(ParseError::AstTooDeep {
            limit, observed, ..
        }) = *answer
        {
            refused = refused.saturating_add(1);
            assert_eq!(limit, MAX_AST_DEPTH, "seed {seed:#x}: the applied bound");
            assert!(
                observed > limit,
                "seed {seed:#x}: observed {observed} does not exceed the bound {limit}"
            );
        }
        Ok(())
    })?;
    assert!(
        refused >= SEEDS.saturating_div(8),
        "only {refused} of {SEEDS} seeds produced a depth refusal; the generator is not nesting \
         past the ceiling often enough to test the bound"
    );
    Ok(())
}

#[test]
/// A source small enough to be inside every ceiling is accepted, so the family
/// exercises the accepting arm as well as the refusing ones.
fn a_source_inside_every_ceiling_is_accepted() -> TestResult {
    let mut accepted = 0_usize;
    sweep(4, |_seed, source, shape, answer| {
        let depth_ish = source.matches('{').count().max(source.matches('}').count());
        if depth_ish > 16 {
            return Ok(());
        }
        if answer.is_ok() {
            accepted = accepted.saturating_add(1);
        } else if shape == Shape::Balanced {
            // A balanced shape inside the ceiling is valid Rust by construction,
            // so a refusal here is a real defect rather than a hostile input.
            return Err(format!("balanced source refused as {}", arm(answer)).into());
        }
        Ok(())
    })?;
    assert!(
        accepted > 0,
        "no seed produced a source small enough to parse; the depth draw never went low"
    );
    Ok(())
}

#[test]
/// Every arm the family reaches is one this crate can name, and no seed reaches
/// an arm that is not a refusal or a tree.
fn no_seed_reaches_an_unnameable_arm() -> TestResult {
    sweep(5, |seed, _source, _shape, answer| -> TestResult {
        let named = arm(answer);
        assert!(
            !named.starts_with("unclassified:"),
            "seed {seed:#x}: the answer was {named}, a variant this test does not name; \\
             a new arm has to be classified here rather than counted as any refusal"
        );
        Ok(())
    })
}

#[test]
/// The same seed replays to the same trace hash.
fn the_same_seed_replays_to_the_same_trace() -> TestResult {
    for index in 0..SEEDS {
        let seed = seed_for(6, index);
        let first = scenario(seed)?;
        assert!(
            !first.is_empty(),
            "seed {seed:#x}: the scenario recorded nothing"
        );
        assert_eq!(
            first.hash(),
            scenario(seed)?.hash(),
            "seed {seed:#x}: the replay diverged"
        );
    }
    Ok(())
}

#[test]
/// The family's outcomes vary, so the replay receipt above is a receipt about a
/// varying input rather than about one outcome written ninety-six times.
///
/// The trace records the *outcome* — the shape, the byte count, the arm, and the
/// tree's size when there is one — so distinct traces is a coarser measure than
/// distinct sources: several sources that land on the same arm at the same size
/// are one trace, correctly. The claim is therefore two-part and each part is
/// checked: many distinct traces, and more than one arm among them. A generator
/// that had stopped varying would collapse both.
fn different_seeds_write_different_traces() -> TestResult {
    let mut hashes: BTreeSet<u64> = BTreeSet::new();
    for index in 0..SEEDS {
        hashes.insert(scenario(seed_for(7, index))?.hash());
    }
    let distinct = u64::try_from(hashes.len())?;
    assert!(
        distinct.saturating_mul(2) >= SEEDS,
        "only {distinct} distinct traces from {SEEDS} seeds"
    );
    let mut arms: BTreeSet<String> = BTreeSet::new();
    sweep(9, |_seed, _source, _shape, answer| {
        arms.insert(arm(answer));
        Ok(())
    })?;
    assert!(
        arms.len() >= 3,
        "the family only reached {arms:?}; a family whose bounds are all exercised has to \
         reach an accepting arm and at least two different refusals"
    );
    Ok(())
}

#[test]
/// The nesting draw varies enough to produce a spread of tree depths, so a
/// single arm is not standing in for the family.
fn the_generated_trees_have_a_spread_of_depths() -> TestResult {
    let mut depths: BTreeSet<usize> = BTreeSet::new();
    for index in 0..SEEDS {
        let seed = seed_for(8, index);
        let mut rng = Rng::new(seed);
        let (source, _) = write_source(&mut rng)?;
        let tree = lgwks_ast::parse(&source, Language::Rust);
        depths.insert(inspect_ast(&tree.root(), None).max_depth);
    }
    assert!(
        depths.len() >= 8,
        "only {} distinct tree depths across {SEEDS} seeds; the nesting draw is not varying",
        depths.len()
    );
    Ok(())
}
