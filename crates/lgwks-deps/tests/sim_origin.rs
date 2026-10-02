//! Deterministic simulation of origin admission (issue #158 A1).
//!
//! One seed drives the family's sequence of approved/observed origin pairs.
//! The verdict for each pair is folded into a trace; running the same seed
//! twice must produce the same trace hash, and a failure prints the seed. The
//! invariant under test is that an observed origin is admitted exactly when it
//! equals the approved origin — never merely because it shares the class.

use std::collections::hash_map::DefaultHasher;
use std::error::Error;
use std::hash::{Hash, Hasher};

use lgwks_deps::contract::Contract;
use lgwks_deps::metadata::{self, DirectEdge};
use lgwks_deps::{Refusal, audit_direct};

type TestResult = Result<(), Box<dyn Error>>;

/// A tiny LCG so the seed fully determines the sequence with no wall clock, no
/// OS entropy, and no dependency. `wrapping_*` keeps the arithmetic total.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed ^ 0x9E37_79B9_7F4A_7C15)
    }

    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0
    }

    fn pick<'a, T>(&mut self, values: &'a [T]) -> &'a T {
        let len = u64::try_from(values.len()).unwrap_or(u64::MAX).max(1);
        let index = usize::try_from(self.next().checked_rem(len).unwrap_or(0)).unwrap_or(0);
        &values[index]
    }
}

/// One `app` → `engine` edge for `source`/`path` through the public parser.
fn edge(source: Option<&str>, path: Option<&str>) -> Result<DirectEdge, Box<dyn Error>> {
    let source_json = source.map_or("null".to_owned(), |value| format!("\"{value}\""));
    let path_json = path.map_or("null".to_owned(), |value| format!("\"{value}\""));
    let document = format!(
        r#"{{"packages":[{{"id":"app","name":"app","repository":null,"manifest_path":"/repo/Cargo.toml","dependencies":[{{"name":"engine","source":{source_json},"req":"1.0","kind":null,"optional":false,"path":{path_json}}}]}}],"workspace_members":["app"]}}"#
    );
    let mut edges = metadata::parse(&document)?;
    edges
        .pop()
        .ok_or_else(|| "the fixture edge must parse".into())
}

/// A register approving `engine` with the given source class and exact origin.
fn register(source: &str, origin: &str) -> Result<Contract, Box<dyn Error>> {
    let text = format!(
        concat!(
            "[policy]\nenforce = true\n\n",
            "[[approved]]\n",
            "crate = \"engine\"\ntier = \"boundary\"\nversion = \"1.0\"\nowner = \"app\"\n",
            "capability = \"engine.core\"\nsource = \"{source}\"\norigin = \"{origin}\"\n",
            "allowed_consumers = \"app\"\nallowed_kinds = \"normal\"\n",
            "reason = \"The engine supplies a capability the standard library cannot express.\"\n",
            "approved_by = \"reviewer\"\napproved_on = \"2026-09-30\"\nreview = \"tests/sim_origin.rs\"\n",
        ),
        source = source,
        origin = origin,
    );
    Ok(Contract::parse(&text)?)
}

/// 0 admitted, 1 origin drift, 2 any other refusal.
fn verdict(refusals: &[Refusal]) -> u8 {
    match refusals.first() {
        None => 0,
        Some(&Refusal::OriginDrift { .. }) => 1,
        Some(_) => 2,
    }
}

/// Runs the whole family for `seed` and returns (trace hash, admitted, drift).
fn run(seed: u64) -> Result<(u64, usize, usize), Box<dyn Error>> {
    let repos = [
        "git+https://a.example/engine",
        "git+https://b.example/engine",
    ];
    let revs = ["", "?rev=abcdef", "?rev=123456"];
    let registries = [
        "registry+https://approved.example/index",
        "registry+https://other.example/index",
    ];
    let paths = ["../vendor/engine", "../vendor/other"];

    let mut rng = Rng::new(seed);
    let mut hasher = DefaultHasher::new();
    let mut admitted = 0_usize;
    let mut drifted = 0_usize;

    for _ in 0..256 {
        let family = rng.next().checked_rem(3).unwrap_or(0);
        let (approved, observed, edges) = match family {
            0 => {
                let approved = format!("{}{}", rng.pick(&repos), rng.pick(&revs));
                let observed = format!("{}{}", rng.pick(&repos), rng.pick(&revs));
                let edges = vec![edge(Some(&observed), None)?];
                (approved, observed, edges)
            }
            1 => {
                let approved = (*rng.pick(&registries)).to_owned();
                let observed = (*rng.pick(&registries)).to_owned();
                let edges = vec![edge(Some(&observed), None)?];
                (approved, observed, edges)
            }
            _ => {
                let approved = (*rng.pick(&paths)).to_owned();
                let observed = (*rng.pick(&paths)).to_owned();
                let edges = vec![edge(None, Some(&observed))?];
                (approved, observed, edges)
            }
        };
        let class = if approved.starts_with("git+") {
            "git"
        } else if approved.starts_with("registry+") {
            "registry"
        } else {
            "path"
        };
        let register = register(class, &approved)?;
        let code = verdict(&audit_direct(&edges, &register));
        let expected = u8::from(observed != approved);
        assert_eq!(
            code, expected,
            "seed {seed}: observed {observed} against approved {approved} gave code {code}"
        );
        if code == 0 {
            admitted = admitted.saturating_add(1);
        } else if code == 1 {
            drifted = drifted.saturating_add(1);
        }
        code.hash(&mut hasher);
    }
    Ok((hasher.finish(), admitted, drifted))
}

#[test]
fn origin_admission_is_deterministic_and_exact() -> TestResult {
    let mut total_admitted = 0_usize;
    let mut total_drifted = 0_usize;
    for seed in 0..64_u64 {
        let (first, admitted, drifted) = run(seed)?;
        let (second, _, _) = run(seed)?;
        assert_eq!(
            first, second,
            "seed {seed} produced two different trace hashes"
        );
        total_admitted = total_admitted.saturating_add(admitted);
        total_drifted = total_drifted.saturating_add(drifted);
    }
    assert!(
        total_admitted > 0 && total_drifted > 0,
        "the sweep must exercise admitted and drifted origins: {total_admitted} admitted, {total_drifted} drifted"
    );
    Ok(())
}
