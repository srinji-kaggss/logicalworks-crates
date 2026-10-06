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

use crate::sim;

use sim::{EmptyTable, Rng};

type TestResult = Result<(), Box<dyn Error>>;

/// One `app` → `engine` edge for `source`/`path` through the public parser.
fn edge(source: Option<&str>, path: Option<&str>) -> Result<DirectEdge, Box<dyn Error>> {
    let source_json = source.map_or("null".to_owned(), |value| format!("\"{value}\""));
    let path_json = path.map_or("null".to_owned(), |value| format!("\"{value}\""));
    let document = format!(
        r#"{{"packages":[{{"id":"app","name":"app","repository":null,"manifest_path":"/repo/Cargo.toml","dependencies":[{{"name":"engine","source":{source_json},"req":"1.0","kind":null,"optional":false,"path":{path_json}}}]}},{{"id":"lic-engine","name":"engine","license":"MIT OR Apache-2.0","manifest_path":"/dep/Cargo.toml","dependencies":[]}}],"workspace_members":["app"]}}"#
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
            "[policy]\nenforce = true\naccepted_licenses = \"MIT, Apache-2.0\"\n\n",
            "[[approved]]\n",
            "crate = \"engine\"\ntier = \"boundary\"\nversion = \"1.0\"\nowner = \"app\"\n",
            "capability = \"engine.core\"\nlicense = \"MIT OR Apache-2.0\"\nsource = \"{source}\"\norigin = \"{origin}\"\n",
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

/// The tables one case draws from, named so the draw sites below read as what
/// they choose from rather than as index arithmetic.
struct OriginTables<'a> {
    /// Git repository sources.
    repos: &'a [&'a str],
    /// Revision policies admitted for a Git source.
    revs: &'a [&'a str],
    /// Registry sources.
    registries: &'a [&'a str],
    /// External path authorities.
    paths: &'a [&'a str],
}

/// One drawn case: the approved origin, the observed origin, and the edge that
/// carries the observed one.
///
/// Every draw for the case happens here so the run loop states one step rather
/// than a chain, and so a refusal names the draw that refused.
fn draw_case(rng: &mut Rng, tables: OriginTables<'_>) -> Result<OriginCase, Box<dyn Error>> {
    let family = rng
        .next_u64()
        .checked_rem(FAMILIES)
        .ok_or("the family count must divide the draw")?;
    let (approved, observed, edges) = match family {
        0 => {
            let approved = git_origin(rng, tables.repos, tables.revs)?;
            let observed = git_origin(rng, tables.repos, tables.revs)?;
            let edges = vec![edge(Some(&observed), None)?];
            (approved, observed, edges)
        }
        1 => {
            let approved = (*rng.pick_named("registry sources", tables.registries)?).to_owned();
            let observed = (*rng.pick_named("registry sources", tables.registries)?).to_owned();
            let edges = vec![edge(Some(&observed), None)?];
            (approved, observed, edges)
        }
        _ => {
            let approved = (*rng.pick_named("path authorities", tables.paths)?).to_owned();
            let observed = (*rng.pick_named("path authorities", tables.paths)?).to_owned();
            let edges = vec![edge(None, Some(&observed))?];
            (approved, observed, edges)
        }
    };
    Ok(OriginCase {
        approved,
        observed,
        edges,
    })
}

/// The approved origin, the observed origin, and the edge carrying the observed
/// one, as one draw produced them.
struct OriginCase {
    /// The origin the register approves.
    approved: String,
    /// The origin the edge declares.
    observed: String,
    /// The edge carrying `observed`.
    edges: Vec<lgwks_deps::metadata::DirectEdge>,
}

/// One drawn Git origin: a repository and the revision policy admitted for it.
///
/// One draw per part, each bound before the next, so a refusal names the draw
/// that failed instead of appearing inside a formatted string.
fn git_origin(rng: &mut Rng, repos: &[&str], revs: &[&str]) -> Result<String, EmptyTable> {
    let repo = rng.pick_named("git repositories", repos)?;
    let revision = rng.pick_named("revision policies", revs)?;
    Ok(format!("{repo}{revision}"))
}

/// How many origin families the run draws between: a Git repository plus its
/// admitted revision policy, a registry source, and an external path authority.
/// Named so the modulus the family draw reduces by is one fact about the suite
/// rather than a literal beside the arm it selects.
const FAMILIES: u64 = 3;

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
        let case = OriginTables {
            repos: &repos,
            revs: &revs,
            registries: &registries,
            paths: &paths,
        };
        let OriginCase {
            approved,
            observed,
            edges,
        } = draw_case(&mut rng, case)?;
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
