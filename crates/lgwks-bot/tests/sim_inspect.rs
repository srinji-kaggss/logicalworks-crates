#![cfg(feature = "inspect")]
//! Deterministic simulation of the inspection input space (R8).
//!
//! One seed drives the generated subject, so a failure prints its seed and the
//! same seed reproduces the same bytes, the same report and the same trace.
//! The walk itself is pure and synchronous — it owns no clock, no socket and no
//! disk — so the simulated dimension is the subject/rule space, and every
//! assertion is against an independent model of the shipped rules.
//!
//! This file is where half or more of the tests added for #150 live, as the
//! worker contract requires.

use lgwks_bot::inspect::{Budgets, IncompleteReason, InspectRequest, Verdict, inspect};

/// The methods the generator can place in a call position.
const METHODS: [&str; 5] = ["unwrap", "expect", "ok", "wrapped", "unwrap_or_default"];

/// Whether the shipped `rust/no-unwrap` rule matches `method`.
fn model_matches(method: &str) -> bool {
    matches!(method, "unwrap" | "expect")
}

/// A splitmix64-step PRNG: deterministic, allocation-free, no dependency.
fn next(seed: &mut u64) -> u64 {
    *seed = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut mixed = *seed;
    mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    mixed ^ (mixed >> 31)
}

/// A value in `0..bound` from `seed`, with no modulo operator.
fn pick(seed: &mut u64, bound: usize) -> usize {
    usize::try_from(next(seed) & 0xFFFF)
        .unwrap_or(0)
        .checked_rem(bound)
        .unwrap_or(0)
}

/// One generated subject and the violation count a faithful rule set reports.
fn generate(seed: u64) -> (String, usize) {
    let mut state = seed;
    let line_count = 2_usize.saturating_add(pick(&mut state, 6));
    let mut expected = 0_usize;
    let mut body = String::new();
    for index in 0..line_count {
        let method = METHODS[pick(&mut state, METHODS.len())];
        if model_matches(method) {
            expected = expected.saturating_add(1);
        }
        let line = format!("    let value{index} = input.{method}();\n");
        body.push_str(&line);
    }
    (format!("fn generated() {{\n{body}}}\n"), expected)
}

/// A subject with `count` functions, each carrying one violation.
fn dense_subject(count: usize) -> String {
    (0..count)
        .map(|index| format!("fn f{index}() {{ let value = input.unwrap(); }}\n"))
        .collect()
}

/// A subject nested `depth` blocks deep.
fn deep_subject(depth: usize) -> String {
    let mut source = String::from("fn f() {");
    source.push_str(&" {".repeat(depth));
    source.push_str(&"}".repeat(depth.saturating_add(1)));
    source.push('\n');
    source
}

#[test]
fn sim_seeded_fragments_match_the_rule_model() {
    for seed in 0..128_u64 {
        let (source, expected) = generate(seed);
        let report = inspect(&InspectRequest::new("gen.rs", &source));
        let observed = report
            .findings()
            .iter()
            .filter(|found| found.rule_id() == "rust/no-unwrap")
            .count();
        assert_eq!(
            observed, expected,
            "seed {seed}: model and rule disagree on\n{source}"
        );
        for found in report.findings() {
            let (start, end) = found.byte_range();
            let text = source.get(start..end).unwrap_or("");
            assert!(
                model_matches(text),
                "seed {seed}: span {text:?} is not a modelled match"
            );
        }
    }
}

#[test]
fn sim_same_seed_same_trace() -> Result<(), Box<dyn std::error::Error>> {
    for seed in 0..64_u64 {
        let (source, _) = generate(seed);
        let first = inspect(&InspectRequest::new("gen.rs", &source));
        let second = inspect(&InspectRequest::new("gen.rs", &source));
        assert_eq!(first, second, "seed {seed}: identical input diverged");
        assert_eq!(
            first.to_json()?,
            second.to_json()?,
            "seed {seed}: trace drifted"
        );
    }
    Ok(())
}

#[test]
fn sim_clean_and_violating_seeds_are_distinguishable() {
    for seed in 0..128_u64 {
        let (source, expected) = generate(seed);
        let report = inspect(&InspectRequest::new("gen.rs", &source));
        if expected == 0 {
            assert!(
                matches!(report.verdict(), Verdict::Clean { .. }),
                "seed {seed}: a modelled-clean subject was not clean: {:?}",
                report.verdict()
            );
        } else {
            assert!(
                matches!(report.verdict(), Verdict::Violations { count } if *count == expected),
                "seed {seed}: expected {expected} violations, got {:?}",
                report.verdict()
            );
        }
    }
}

#[test]
fn sim_permuting_statements_preserves_the_finding_set() {
    for seed in 0..32_u64 {
        let mut state = seed;
        let count = 1_usize.saturating_add(pick(&mut state, 8));
        let forwards = dense_subject(count);
        let mut lines: Vec<&str> = forwards.lines().collect();
        lines.reverse();
        let reversed = lines.join("\n");
        let first = inspect(&InspectRequest::new("gen.rs", &forwards));
        let second = inspect(&InspectRequest::new("gen.rs", &reversed));
        let mut first_ids: Vec<&str> = first
            .findings()
            .iter()
            .map(|found| found.rule_id())
            .collect();
        let mut second_ids: Vec<&str> = second
            .findings()
            .iter()
            .map(|found| found.rule_id())
            .collect();
        first_ids.sort_unstable();
        second_ids.sort_unstable();
        assert_eq!(
            first_ids, second_ids,
            "seed {seed}: statement order changed the finding set"
        );
    }
}

#[test]
fn sim_node_budget_tiers_refuse_deterministically() {
    let wide = dense_subject(32);
    for budget in 1..9_usize {
        let request =
            InspectRequest::new("wide.rs", &wide).budgets(Budgets::new().with_nodes(budget));
        let first = inspect(&request);
        let second = inspect(&request);
        assert_eq!(first, second, "budget {budget}: iterative refusal diverged");
        assert!(
            matches!(
                first.verdict(),
                Verdict::Incomplete {
                    reason: IncompleteReason::NodeBudgetExceeded { .. }
                }
            ),
            "budget {budget}: a wide tree must refuse: {:?}",
            first.verdict()
        );
        assert!(
            first.resources().nodes <= budget.saturating_add(1),
            "budget {budget}: an eager walk would exceed the cap, observed {}",
            first.resources().nodes
        );
    }
}

#[test]
fn sim_depth_budget_refuses_a_deep_tree() {
    for depth in 1..24_usize {
        let source = deep_subject(depth);
        let request = InspectRequest::new("deep.rs", &source).budgets(Budgets::new().with_depth(3));
        let report = inspect(&request);
        assert!(
            matches!(
                report.verdict(),
                Verdict::Incomplete {
                    reason: IncompleteReason::DepthBudgetExceeded { .. }
                }
            ),
            "depth {depth}: a 3-deep budget must refuse: {:?}",
            report.verdict()
        );
        assert!(
            report.resources().max_depth <= 3_usize.saturating_add(1),
            "depth {depth}: the walk charged past its budget plus its overflow witness: {}",
            report.resources().max_depth
        );
    }
}

#[test]
fn sim_finding_density_is_capped_deterministically() {
    let source = dense_subject(64);
    for cap in 1..6_usize {
        let request =
            InspectRequest::new("dense.rs", &source).budgets(Budgets::new().with_findings(cap));
        let first = inspect(&request);
        let second = inspect(&request);
        assert_eq!(first, second, "cap {cap}: iterative refusal diverged");
        assert_eq!(
            first.resources().findings,
            cap,
            "cap {cap}: retained findings must equal the cap"
        );
        assert!(
            matches!(
                first.verdict(),
                Verdict::Incomplete {
                    reason: IncompleteReason::FindingsBudgetExceeded { .. }
                }
            ),
            "cap {cap}: a capped walk is incomplete: {:?}",
            first.verdict()
        );
    }
}

#[test]
fn sim_output_budget_is_charged_before_retention() {
    let source = dense_subject(8);
    let request =
        InspectRequest::new("out.rs", &source).budgets(Budgets::new().with_output_bytes(4));
    let first = inspect(&request);
    let second = inspect(&request);
    assert_eq!(first, second, "output refusal diverged");
    assert!(
        matches!(
            first.verdict(),
            Verdict::Incomplete {
                reason: IncompleteReason::OutputBudgetExceeded { .. }
            }
        ),
        "a tiny output budget must refuse before retaining: {:?}",
        first.verdict()
    );
    assert_eq!(
        first.resources().output_bytes,
        0,
        "nothing may be charged once the first finding is refused"
    );
}

#[test]
fn sim_digest_is_content_addressed_not_artifact_addressed() {
    let left = inspect(&InspectRequest::new("a.rs", "fn f() { g().unwrap(); }"));
    let right = inspect(&InspectRequest::new("b.rs", "fn f() { g().unwrap(); }"));
    assert_eq!(
        left.subject_digest(),
        right.subject_digest(),
        "the digest is a property of the bytes, not the artifact name"
    );
    let other = inspect(&InspectRequest::new("a.rs", "fn f() {}"));
    assert_ne!(
        left.subject_digest(),
        other.subject_digest(),
        "different bytes must have different digests"
    );
}

#[test]
fn sim_unicode_and_substring_identifiers_never_false_positive() {
    let source = "fn f() { let a = wrapped(); let b = my_unwrap(); let c = unwrap_or(1); let d = unwrapped(); let é = 1; }\n";
    let report = inspect(&InspectRequest::new("unicode.rs", source));
    assert!(
        matches!(report.verdict(), Verdict::Clean { .. }),
        "only an exact `unwrap`/`expect` identifier matches: {:?}",
        report.verdict()
    );
}

#[test]
fn sim_wide_subjects_stay_within_the_node_budget() {
    let source = dense_subject(256);
    let request = InspectRequest::new("wide.rs", &source);
    let first = inspect(&request);
    let second = inspect(&request);
    assert_eq!(first, second, "a wide subject diverged across runs");
    assert!(
        first.resources().nodes <= lgwks_ast::MAX_AST_NODES,
        "the walk must stay within the declared node bound"
    );
    assert!(
        first.resources().nodes > 0,
        "a wide subject must actually be walked"
    );
}

#[test]
fn sim_exhausted_budgets_never_read_as_clean() {
    for seed in 0..64_u64 {
        let (source, _) = generate(seed);
        let request = InspectRequest::new("gen.rs", &source).budgets(Budgets::new().with_nodes(1));
        let report = inspect(&request);
        assert!(
            !matches!(report.verdict(), Verdict::Clean { .. }),
            "seed {seed}: a node-exhausted walk must never be clean: {:?}",
            report.verdict()
        );
    }
}

#[test]
fn sim_repeated_inspection_carries_no_state() {
    let source = dense_subject(16);
    let request = InspectRequest::new("repeat.rs", &source);
    let baseline = inspect(&request);
    for _ in 0..100 {
        assert_eq!(
            inspect(&request),
            baseline,
            "the operation must carry no state between calls"
        );
    }
}
