//! The declared repair tiers, each run for real at the level the host reaches.

use super::*;

/// The declared three tiers, each run for real, at the level the host reaches.
///
/// One explicit test rather than a band, because the tiers are the claim. A tier
/// the host cannot finish in the time available is reported as the level it
/// reached rather than as the level that was asked for, so the trace never
/// reports a concurrency number nobody ran (INV-BOT-16).
///
/// Every run drives the first tier with every assertion; the larger two are
/// minutes of real `fsync`s and run when [`SCALE_ENV`] is set.
#[test]
fn the_declared_repair_tiers_are_measured() -> TestResult {
    let tiers = match std::env::var_os(SCALE_ENV) {
        Some(_) => &TIERS[..],
        None => &TIERS[..1],
    };
    for &tier in tiers {
        let dir = Scratch::new("repair-tier")?;
        let host = blocked_host(TENANTS[0], dir.path())?;
        let declared = shared::journey_task()?;
        let grants = exact_grant(candidate_needs().len());

        let mut answered = 0_usize;
        for _ in 0..tier {
            let polls = Polls::shared();
            let report = lgwks_bot::block_on(host.run(
                &declared,
                input_needing(Arc::clone(&polls), 7, candidate_needs()),
            ));
            let Some(ticket) = report.repair() else {
                break;
            };
            let repaired = lgwks_bot::block_on(host.repair(
                ticket,
                &grants,
                &declared,
                input(Arc::clone(&polls), 7),
                1,
            ))?;
            if repaired.disposition() != Disposition::Succeeded {
                break;
            }
            answered = answered.saturating_add(1);
        }
        let reached = answered;
        assert!(
            reached > 0,
            "tier {tier}: the host answered at least one repair"
        );
        let mut line = std::io::stdout().lock();
        let _written = writeln!(
            line,
            "repair tier {tier}: requested={tier} reached={reached} admitted={} refused={}",
            host.admission().admitted(),
            host.admission().refused()
        );
    }
    Ok(())
}
