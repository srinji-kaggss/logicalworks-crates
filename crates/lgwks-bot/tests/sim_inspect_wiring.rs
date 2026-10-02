//! Deterministic simulation of the inspection *wiring* (R8, #150).
//!
//! The core operation's input space is simulated in `sim_inspect.rs`. This file
//! simulates the doors into it: one seed drives the generated subject and the
//! chosen budgets, and every entry point — the free function, the `Query`, the
//! `Observe` source behind a bot, and the `Host` task — must return the same
//! report. A failure prints its seed, and the same seed reproduces the same
//! source, the same reports and the same trace hash.
#![cfg(all(feature = "inspect", feature = "script"))]

mod inspect_support;

use inspect_support::sim::{Rng, Trace};
use inspect_support::{INSPECT_DOMAINS, TestResult, artifact_for, observe_once, scope};
use lgwks_bot::domain::inspect::{InspectionJob, Inspector, inspection_task};
use lgwks_bot::inspect::{Budgets, IncompleteReason, InspectRequest, Inspection, Verdict, inspect};
use lgwks_bot::spec::Bot;
use lgwks_bot::task::Host;
use lgwks_bot::verb::Query;
use lgwks_bot::{Admission, Cap, GrantSet, Need};

/// The call sites the generator may place, each paired with whether the shipped
/// `rust/no-unwrap` rule matches it.
const CALL_SITES: [(&str, bool); 5] = [
    ("unwrap", true),
    ("expect", true),
    ("ok", false),
    ("wrapped", false),
    ("unwrap_or_default", false),
];

/// A draw in `0..bound` from the shared generator, without an `as` cast.
fn draw(rng: &mut Rng, bound: usize) -> usize {
    usize::try_from(rng.below(u32::try_from(bound).unwrap_or(1))).unwrap_or(0)
}

/// One generated subject and the violation count a faithful rule model reports.
fn source_for(seed: u64) -> (String, usize) {
    let mut rng = Rng::new(seed ^ 0x0000_1500_0000_0000);
    let count = rng.between(2, 8);
    let mut expected = 0_usize;
    let mut body = String::new();
    for index in 0..count {
        let (method, matches) = CALL_SITES[draw(&mut rng, CALL_SITES.len())];
        expected = expected.saturating_add(usize::from(matches));
        let line = format!("    let v{index} = input.{method}();\n");
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

/// Run the free operation, the `Query` verb and the `Host` task over one
/// artifact and return the three reports.
fn three_doors(artifact: &str, source: &str) -> TestResult<(Inspection, Inspection, Inspection)> {
    let direct = inspect(&InspectRequest::new(artifact, source));
    let auth = GrantSet::empty().issue(&[])?;
    let job = InspectionJob::new(artifact, source);
    let query = lgwks_std::task::block_on(Inspector::new().query((auth, &job)))?;
    let host = Host::builder("sim-doors")?.build()?;
    let task = inspection_task("inspect")?;
    let report = host.block_on(&task, InspectionJob::new(artifact, source))?;
    let by_task = report.into_result()?;
    Ok((direct, query, by_task))
}

#[test]
fn sim_seeded_subjects_agree_across_every_entry_point() -> TestResult<()> {
    for seed in 0..48_u64 {
        let (source, _) = source_for(seed);
        let (direct, query, by_task) = three_doors("gen.rs", &source)?;
        assert_eq!(direct, query, "seed {seed}: the Query verb diverged");
        assert_eq!(
            direct, by_task,
            "seed {seed}: the Host task diverged from the free function"
        );

        // The Observe source behind a real bot reads the artifact from disk; the
        // report must be the same report as the bytes handed over directly.
        let artifact = artifact_for(&format!("sim{seed}"), &source)?;
        let observed = observe_once(&artifact, "sim")?;
        assert_eq!(
            observed,
            inspect(&InspectRequest::new(&artifact, &source)),
            "seed {seed}: the source's file read diverged from the bytes"
        );
    }
    Ok(())
}

#[test]
fn sim_same_seed_same_trace_hash() -> TestResult<()> {
    for seed in 0..32_u64 {
        let (source, _) = source_for(seed);
        let (first, _, _) = three_doors("gen.rs", &source)?;
        let (second, _, _) = three_doors("gen.rs", &source)?;
        assert_eq!(first, second, "seed {seed}: identical input diverged");

        let mut trace = Trace::new();
        trace.record("first");
        trace.record(&first.to_json()?);
        trace.record("second");
        trace.record(&second.to_json()?);
        let mut replay = Trace::new();
        replay.record("first");
        replay.record(&first.to_json()?);
        replay.record("second");
        replay.record(&second.to_json()?);
        assert_eq!(
            trace.hash(),
            replay.hash(),
            "seed {seed}: the trace hash drifted"
        );
    }
    Ok(())
}

#[test]
fn sim_seeded_budget_tiers_agree_across_entry_points() -> TestResult<()> {
    let wide = dense_subject(32);
    for seed in 0..16_u64 {
        let mut rng = Rng::new(seed ^ 0x0000_0000_1500_0000);
        let node_cap = rng.between(1, 8);
        let job = InspectionJob::new("wide.rs", &wide)
            .with_budgets(Budgets::new().with_nodes(usize::try_from(node_cap).unwrap_or(1)));
        let direct = inspect(
            &InspectRequest::new("wide.rs", &wide)
                .budgets(Budgets::new().with_nodes(usize::try_from(node_cap).unwrap_or(1))),
        );
        let auth = GrantSet::empty().issue(&[])?;
        let query = lgwks_std::task::block_on(Inspector::new().query((auth, &job)))?;
        assert_eq!(
            direct, query,
            "seed {seed}: a budget refusal must agree across entry points"
        );
        assert!(
            matches!(
                query.verdict(),
                Verdict::Incomplete {
                    reason: IncompleteReason::NodeBudgetExceeded { .. }
                }
            ),
            "seed {seed}: a wide tree under a tiny node cap must refuse: {:?}",
            query.verdict()
        );
    }
    Ok(())
}

#[test]
fn sim_seeded_admission_needs_are_deterministic() -> TestResult<()> {
    for seed in 0..16_u64 {
        let (source, _) = source_for(seed);
        let artifact = artifact_for(&format!("needs{seed}"), &source)?;
        let spec = inspect_support::spec_for(&artifact)?;
        let first = Bot::from_spec(&spec, &INSPECT_DOMAINS, &GrantSet::empty(), scope()?);
        let second = Bot::from_spec(&spec, &INSPECT_DOMAINS, &GrantSet::empty(), scope()?);
        let rendered = |result: &Result<Bot, Admission>| format!("{result:?}");
        assert_eq!(
            rendered(&first),
            rendered(&second),
            "seed {seed}: admission was not deterministic"
        );
        let Err(Admission::Needs(needs)) = first else {
            return Err(format!("seed {seed}: expected a need set").into());
        };
        let need = needs
            .needs()
            .first()
            .cloned()
            .ok_or("the need set must carry the missing capability")?;
        match need {
            Need::MissingCapability { capability, .. } => {
                assert_eq!(capability, Cap::fs(), "seed {seed}: the need is `bot.fs`");
            }
            other => {
                return Err(
                    format!("seed {seed}: expected MissingCapability, got {other:?}").into(),
                );
            }
        }
    }
    Ok(())
}

#[test]
fn sim_seeded_multitenant_reports_stay_isolated() -> TestResult<()> {
    for seed in 0..16_u64 {
        let (alpha_source, _) = source_for(seed);
        let (beta_source, _) = source_for(seed ^ 0x1111_2222_3333_4444);
        let alpha_host = Host::builder(&format!("alpha{seed}"))?.build()?;
        let beta_host = Host::builder(&format!("beta{seed}"))?.build()?;
        let task = inspection_task("inspect")?;
        let run = |host: &Host, source: &str| -> TestResult<Inspection> {
            let report = host.block_on(&task, InspectionJob::new("shared.rs", source))?;
            Ok(report.into_result()?)
        };
        let alpha = run(&alpha_host, &alpha_source)?;
        let beta = run(&beta_host, &beta_source)?;
        assert_eq!(
            alpha,
            inspect(&InspectRequest::new("shared.rs", &alpha_source)),
            "seed {seed}: tenant alpha's report is not about alpha's bytes"
        );
        assert_eq!(
            beta,
            inspect(&InspectRequest::new("shared.rs", &beta_source)),
            "seed {seed}: tenant beta's report is not about beta's bytes"
        );
        if alpha_source != beta_source {
            assert_ne!(
                alpha.subject_digest(),
                beta.subject_digest(),
                "seed {seed}: two tenants' reports collided on one artifact name"
            );
        }
    }
    Ok(())
}

#[test]
fn sim_seeded_file_read_matches_the_supplied_bytes() -> TestResult<()> {
    for seed in 0..32_u64 {
        let (source, _) = source_for(seed);
        let artifact = artifact_for(&format!("bytes{seed}"), &source)?;
        let direct = inspect(&InspectRequest::new(&artifact, &source));
        let observed = observe_once(&artifact, "bytes")?;
        assert_eq!(
            observed, direct,
            "seed {seed}: the source read different bytes than were written"
        );
    }
    Ok(())
}

#[test]
fn sim_repeated_task_runs_carry_no_state() -> TestResult<()> {
    let (source, _) = source_for(7);
    let host = Host::builder("repeat")?.build()?;
    let task = inspection_task("inspect")?;
    let baseline = host
        .block_on(&task, InspectionJob::new("repeat.rs", &source))?
        .into_result()?;
    for _ in 0..64 {
        let again = host
            .block_on(&task, InspectionJob::new("repeat.rs", &source))?
            .into_result()?;
        assert_eq!(again, baseline, "the task carried state between runs");
    }
    Ok(())
}

#[test]
fn sim_seeded_host_admission_peak_is_bounded() -> TestResult<()> {
    use std::num::NonZeroUsize;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    let source = "fn f() { g().unwrap(); }\n";
    let expected = inspect(&InspectRequest::new("bounded.rs", source));
    for seed in 0..8_u64 {
        let mut rng = Rng::new(seed ^ 0x0000_0000_0000_1500);
        let ceiling = draw(&mut rng, 4).saturating_add(2);
        let jobs = draw(&mut rng, 24).saturating_add(12);
        let host = Host::builder(&format!("bounded{seed}"))?
            .max_concurrent_tasks(NonZeroUsize::new(ceiling).ok_or("a non-zero ceiling")?)
            .default_deadline(Duration::from_secs(30))
            .build()?;
        let task = inspection_task("inspect")?;
        let next = AtomicUsize::new(0);

        let outcomes = std::thread::scope(|scope| {
            let mut handles = Vec::new();
            for _ in 0..ceiling {
                handles.push(scope.spawn(|| -> Result<usize, String> {
                    let mut done = 0_usize;
                    loop {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        if index >= jobs {
                            break;
                        }
                        let report = host
                            .block_on(&task, InspectionJob::new("bounded.rs", source))
                            .map_err(|error| error.to_string())?;
                        let inspection = report.into_result().map_err(|error| error.to_string())?;
                        if inspection != expected {
                            return Err(format!("seed {seed}: a concurrent run diverged"));
                        }
                        done = done.saturating_add(1);
                    }
                    Ok(done)
                }));
            }
            handles
                .into_iter()
                .map(|handle| {
                    handle
                        .join()
                        .map_err(|_| "a worker thread panicked".to_owned())
                })
                .collect::<Vec<_>>()
        });

        let mut total = 0_usize;
        for outcome in outcomes {
            total = total.saturating_add(outcome??);
        }
        assert_eq!(total, jobs, "seed {seed}: every job must run exactly once");
        let peak = host.admission().peak_in_flight();
        assert!(
            peak <= ceiling,
            "seed {seed}: peak in-flight {peak} exceeded the ceiling {ceiling}"
        );
        assert!(
            peak >= 1,
            "seed {seed}: at least one run must have been in flight"
        );
        assert_eq!(
            host.admission().admitted(),
            u64::try_from(jobs).unwrap_or(0),
            "seed {seed}: the host must have admitted every job"
        );
    }
    Ok(())
}

#[test]
fn sim_seeded_node_budget_never_reads_clean_through_the_domain() -> TestResult<()> {
    for seed in 0..16_u64 {
        let (source, _) = source_for(seed);
        let job = InspectionJob::new("gen.rs", &source).with_budgets(Budgets::new().with_nodes(1));
        let auth = GrantSet::empty().issue(&[])?;
        let report = lgwks_std::task::block_on(Inspector::new().query((auth, &job)))?;
        assert!(
            !matches!(report.verdict(), Verdict::Clean { .. }),
            "seed {seed}: a node-exhausted query read as clean: {:?}",
            report.verdict()
        );
    }
    Ok(())
}

#[test]
fn sim_seeded_unicode_never_false_positives_through_the_domain() -> TestResult<()> {
    let source = "fn f() { let a = wrapped(); let b = my_unwrap(); let é = 1; }\n";
    for seed in 0..16_u64 {
        let artifact = artifact_for(&format!("uni{seed}"), source)?;
        let (direct, query, by_task) = three_doors(&artifact, source)?;
        assert_eq!(direct, query, "seed {seed}: unicode diverged on the Query");
        assert_eq!(direct, by_task, "seed {seed}: unicode diverged on the task");
        assert!(
            matches!(direct.verdict(), Verdict::Clean { .. }),
            "seed {seed}: only an exact `unwrap`/`expect` matches: {:?}",
            direct.verdict()
        );
    }
    Ok(())
}
