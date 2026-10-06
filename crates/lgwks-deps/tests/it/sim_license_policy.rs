//! Deterministic simulation of the register-declared licence policy.
//!
//! The gate is a published crate that audits other repositories, so which
//! licences an external dependency may carry is each register's
//! `[policy] accepted_licenses`, never a set compiled into the gate. One seed
//! draws an accepted set and a package's SPDX expression; the model refuses the
//! edge exactly when the expression names an identifier outside the set, and
//! the audit must agree identifier for identifier, in order. A register that
//! declares no set is refused once, whatever the licence. Two registers with
//! different sets judge the same edge each by its own set. The same seed
//! replays to the same trace hash.

use crate::sim::{Trace, receipt};
use std::error::Error;

use lgwks_deps::contract::Contract;
use lgwks_deps::metadata::{self, DirectEdge};
use lgwks_deps::{Refusal, audit_direct};

use crate::sim;

use sim::Rng;

type TestResult = Result<(), Box<dyn Error>>;

/// Seeds per family.
const SEEDS: u64 = 256;

/// Identifiers the generator draws from. `MIT` and `MIT-0` are both here
/// because one is a prefix of the other, and accepting `MIT` must not accept
/// `MIT-0`; `MPL-2.0` because it is the licence the estate's own `lgwks_bot`
/// is published under, which the old compiled-in set refused for everyone.
const UNIVERSE: [&str; 10] = [
    "MIT",
    "MIT-0",
    "Apache-2.0",
    "MPL-2.0",
    "BSD-3-Clause",
    "GPL-3.0-only",
    "Zlib",
    "ISC",
    "0BSD",
    "Unlicense",
];

/// The registry source the approval and the edge both name.
const REGISTRY: &str = "registry+https://github.com/rust-lang/crates.io-index";

/// The `app` → `engine` edge whose package declares `license`.
fn edge(license: &str) -> Result<DirectEdge, Box<dyn Error>> {
    let document = format!(
        r#"{{"packages":[{{"id":"app","name":"app","repository":null,"manifest_path":"/repo/Cargo.toml","dependencies":[{{"name":"engine","source":"{REGISTRY}","req":"1.0","kind":null,"rename":null,"optional":false,"uses_default_features":true,"features":[],"target":null,"path":null}}]}},{{"id":"lic-engine","name":"engine","license":"{license}","manifest_path":"/dep/Cargo.toml","dependencies":[]}}],"workspace_members":["app"]}}"#
    );
    metadata::parse(&document)?
        .pop()
        .ok_or_else(|| "the fixture edge must parse".into())
}

/// A register approving `engine` under `license`, with `accepted` as its
/// declared set or no declaration at all.
fn register(license: &str, accepted: Option<&[&str]>) -> Result<Contract, Box<dyn Error>> {
    let policy = accepted.map_or(String::new(), |set| {
        format!("accepted_licenses = \"{}\"\n", set.join(", "))
    });
    let text = format!(
        concat!(
            "[policy]\nschema = 2\nenforce = true\n{policy}\n",
            "[[approved]]\n",
            "crate = \"engine\"\ntier = \"boundary\"\nversion = \"1.0\"\nowner = \"app\"\n",
            "capability = \"engine.core\"\nlicense = \"{license}\"\nsource = \"registry\"\n",
            "allowed_consumers = \"app\"\nallowed_kinds = \"normal\"\n",
            "reason = \"The engine supplies a capability the standard library cannot express.\"\n",
            "approved_by = \"reviewer\"\napproved_on = \"2026-10-05\"\n",
            "review = \"tests/it/sim_license_policy.rs\"\n",
        ),
        policy = policy,
        license = license,
    );
    Ok(Contract::parse(&text)?)
}

/// A non-empty accepted set drawn from [`UNIVERSE`], in universe order.
fn draw_accepted(rng: &mut Rng) -> Vec<&'static str> {
    // A mask over the ten identifiers, never zero: one through 1023.
    let mask = rng.between(1, 0x3ff);
    UNIVERSE
        .iter()
        .enumerate()
        .filter(|&(index, _)| mask & (1 << index) != 0)
        .map(|(_, name)| *name)
        .collect()
}

/// An SPDX expression of one to three distinct identifiers joined by `OR` or
/// `AND`, and the identifiers in the order written.
fn draw_expression(rng: &mut Rng) -> (String, Vec<&'static str>) {
    let count = match rng.below(3) {
        0 => 1,
        1 => 2,
        _ => 3,
    };
    let mut ids: Vec<&'static str> = Vec::new();
    while ids.len() < count {
        // `UNIVERSE` is a non-empty constant; the arm is what an emptied one
        // means — no identifier is available, so the expression holds only the
        // identifiers already drawn rather than spinning on a table with none.
        let Some(&id) = rng.pick(&UNIVERSE) else {
            break;
        };
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    let joiner = if rng.coin() { " OR " } else { " AND " };
    (ids.join(joiner), ids)
}

/// What the audit must report for `ids` under `accepted`.
fn model(ids: &[&str], accepted: &[&str]) -> Vec<String> {
    ids.iter()
        .filter(|id| !accepted.contains(id))
        .map(|id| (*id).to_owned())
        .collect()
}

/// The licence refusal an audit reported, if any, as its rejected list.
fn rejected_by(refusals: &[Refusal]) -> Result<Option<Vec<String>>, Box<dyn Error>> {
    refusals.iter().try_fold(None, |_, refusal| match *refusal {
        Refusal::LicenseNotAccepted { ref rejected, .. } => Ok(Some(rejected.clone())),
        ref other => Err(format!("an unrelated refusal: {other}").into()),
    })
}

/// One seed's scenario folded into a trace hash.
fn scenario(seed: u64) -> Result<u64, Box<dyn Error>> {
    let mut rng = Rng::new(seed);
    let accepted = draw_accepted(&mut rng);
    let (expression, ids) = draw_expression(&mut rng);
    let refusals = audit_direct(
        &[edge(&expression)?],
        &register(&expression, Some(&accepted))?,
    );
    let expected = model(&ids, &accepted);
    let got = rejected_by(&refusals)?;
    let agrees = if expected.is_empty() {
        got.is_none()
    } else {
        got.as_ref() == Some(&expected)
    };
    let verdict: Result<(), Box<dyn Error>> = if agrees {
        Ok(())
    } else {
        Err(format!(
            "seed {seed:#x}: {expression:?} under {accepted:?} reported {got:?}, model {expected:?}"
        )
        .into())
    };
    verdict?;
    let mut trace = Trace::new();
    trace.record(&format!("{accepted:?}"));
    trace.record(&expression);
    trace.record(&format!("{expected:?}"));
    Ok(receipt(&trace)?)
}

#[test]
/// Every drawn expression is refused exactly on the identifiers outside the
/// register's own set, in the order the expression writes them.
fn the_register_set_decides_every_seeded_expression() -> TestResult {
    let mut refused = 0_u32;
    let mut admitted = 0_u32;
    for index in 0..SEEDS {
        let seed = 0x11ce_05e0_0000_0000 ^ index.wrapping_mul(0x9e37_79b9_7f4a_7c15);
        scenario(seed)?;
        let mut rng = Rng::new(seed);
        let accepted = draw_accepted(&mut rng);
        let (_, ids) = draw_expression(&mut rng);
        if model(&ids, &accepted).is_empty() {
            admitted += 1;
        } else {
            refused += 1;
        }
    }
    assert!(
        refused > 0 && admitted > 0,
        "the family must reach both verdicts: {refused} refused, {admitted} admitted"
    );
    Ok(())
}

#[test]
/// A register with no `accepted_licenses` is one typed refusal, whatever the
/// licence: no set compiled into the gate stands in for the register's.
fn an_undeclared_policy_is_refused_for_every_seeded_licence() -> TestResult {
    for index in 0..SEEDS {
        let mut rng = Rng::new(0x0dec_1a7e ^ index.wrapping_mul(0x2545_f491_4f6c_dd1d));
        let (expression, _) = draw_expression(&mut rng);
        let refusals = audit_direct(&[edge(&expression)?], &register(&expression, None)?);
        assert_eq!(
            refusals,
            vec![Refusal::LicensePolicyUndeclared { approvals: 1 }],
            "{expression:?} under an undeclared policy"
        );
    }
    Ok(())
}

#[test]
/// Two registers over one edge: each judges it by its own set and neither
/// leaks into the other, interleaved in one order and then the reverse.
fn two_registers_judge_one_edge_each_by_its_own_set() -> TestResult {
    for index in 0..SEEDS {
        let mut rng = Rng::new(0x7e4a_0002 ^ index.wrapping_mul(0x9e37_79b9_7f4a_7c15));
        let first = draw_accepted(&mut rng);
        let second = draw_accepted(&mut rng);
        let (expression, ids) = draw_expression(&mut rng);
        let edge = edge(&expression)?;
        let tenants = [
            (register(&expression, Some(&first))?, model(&ids, &first)),
            (register(&expression, Some(&second))?, model(&ids, &second)),
        ];
        for order in [[0_usize, 1], [1, 0]] {
            for tenant in order {
                let (ref contract, ref expected) = tenants[tenant];
                let got = rejected_by(&audit_direct(std::slice::from_ref(&edge), contract))?;
                let expected = (!expected.is_empty()).then(|| expected.clone());
                assert_eq!(got, expected, "tenant {tenant} on {expression:?}");
            }
        }
    }
    Ok(())
}

#[test]
/// The estate's own `lgwks_bot` is published under MPL-2.0: a register that
/// accepts MPL-2.0 admits it, and one that does not refuses it by name.
fn an_mpl_dependency_is_the_registers_decision() -> TestResult {
    let accepting = register("MPL-2.0", Some(&["MIT", "Apache-2.0", "MPL-2.0"]))?;
    let refusing = register("MPL-2.0", Some(&["MIT", "Apache-2.0"]))?;
    assert!(audit_direct(&[edge("MPL-2.0")?], &accepting).is_empty());
    assert_eq!(
        rejected_by(&audit_direct(&[edge("MPL-2.0")?], &refusing))?,
        Some(vec!["MPL-2.0".to_owned()])
    );
    Ok(())
}

#[test]
/// The same seed replays to the same trace; distinct seeds diverge.
fn the_same_seed_replays_and_distinct_seeds_diverge() -> TestResult {
    let mut hashes = std::collections::BTreeSet::new();
    for index in 0..SEEDS {
        let seed = 0x5eed_0003 ^ index.wrapping_mul(0x2545_f491_4f6c_dd1d);
        let first = scenario(seed)?;
        assert_eq!(
            first,
            scenario(seed)?,
            "seed {seed:#x} replayed differently"
        );
        hashes.insert(first);
    }
    assert!(
        hashes.len() > 64,
        "{SEEDS} seeds gave {} traces",
        hashes.len()
    );
    Ok(())
}
