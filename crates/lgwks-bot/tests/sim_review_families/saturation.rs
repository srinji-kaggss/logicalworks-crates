//! The saturation machinery of the review path: one host and one bounded
//! pipeline per tier, receivers sharded at a bound, each receiver's own record
//! asserted.
//!
//! Split out of `sim_review_path.rs` by responsibility (the size law). The three
//! tier tests stay in the binary's root so their names, which the CI matrix and
//! the saturation lane select by, are unchanged; the subject family's
//! saturation test reaches the same `run_saturation_tier`.

use lgwks_bot::domain::gh::Gh;
use lgwks_bot::review::{ReviewOutcome, ReviewRequest};
use lgwks_bot::task::Report;

use super::fake_gh::{FakeGh, Scenario};
use super::{
    BODY, CAPTURE, HEAD, TestResult, gh_spawned_directly, host_named, request, review_task_send,
    why_unknown,
};

/// How many runs of a saturation tier share one receiver.
///
/// The shard exists because the *fixture* is quadratic in the runs that share a
/// receiver, not because the code under test is: every read-back copies the
/// receiver's whole `reviews.jsonl`, so N runs on one pull request pipe ~N²/2
/// records through a `sh` pipe. At 10,000 that is tens of gigabytes of
/// plumbing, and the family stops measuring anything long before it finishes
/// paying it. Past roughly 590 reviews a read-back exceeds this family's
/// [`CAPTURE`] and is refused before it can be decoded; past
/// [`lgwks_bot::domain::gh::MAX_REVIEWS_PER_PULL`] (1,000) it is refused with
/// `ReviewCeiling` instead. Either refusal ends the run `Unknown`, and every
/// inequality this family asserted still holds on a receiver in that state — so
/// the two big tiers were timing a degenerate world and reporting it green.
///
/// One hundred records is roughly 14 KB: inside [`CAPTURE`] with margin, and an
/// order of magnitude inside the review ceiling, so every run at every tier
/// verifies its publication against a complete and decodable history.
///
/// The shard changes the fixture's shape and nothing else. Every run of a tier
/// still goes through **one** [`Host`] with `max_concurrent_tasks(N)` and one
/// bounded pipeline, and the pipeline still admits inputs in order, so up to
/// [`IN_FLIGHT_CAP`] runs are concurrently on a *single* pull request. Merging
/// the receivers back restores the quadratic fixture and the degenerate world.
const RUNS_PER_RECEIVER: usize = 100;

/// The ceiling the saturation family's pipeline runs under, whatever the tier.
///
/// Held well below every tier's run count so the join set is provably bounded
/// rather than merely large, and equal to the number of runs that can be on one
/// pull request at once: the first `IN_FLIGHT_CAP` inputs all land on receiver
/// zero, so the bound is reached rather than approximated.
pub(super) const IN_FLIGHT_CAP: usize = 64;

/// How many runs each receiver of `tier` carries, in order.
///
/// Ceiling division, with the remainder on the last receiver, so the sizes sum
/// to the tier exactly: no run dropped, no receiver over the shard bound. Every
/// step saturates rather than wrapping, because the whole point of these
/// numbers is that a tier is checked against them — a size that silently
/// overflowed into a short slice would shrink the assertion instead of failing
/// it.
fn shard_sizes(tier: usize) -> Vec<usize> {
    let receivers = tier.div_ceil(RUNS_PER_RECEIVER);
    let full = tier.saturating_sub(
        receivers
            .saturating_sub(1)
            .saturating_mul(RUNS_PER_RECEIVER),
    );
    let mut sizes = vec![RUNS_PER_RECEIVER; receivers.saturating_sub(1)];
    sizes.push(full);
    sizes
}

/// Every input of a tier, receiver-major: one receiver's runs, then the next.
///
/// Receiver-major *is* the shard read as an ordering. The pipeline admits
/// inputs in order, so the first [`IN_FLIGHT_CAP`] inputs all land on receiver
/// zero and the pipeline's bound is reached on a single pull request rather
/// than approached somewhere across the tier.
fn saturation_inputs(
    fakes: &[FakeGh],
    sizes: &[usize],
    capture: usize,
) -> Result<Vec<(Gh, ReviewRequest)>, Box<dyn std::error::Error>> {
    let mut work = Vec::new();
    for (fake, shard) in fakes.iter().zip(sizes) {
        for _ in 0..*shard {
            work.push((gh_spawned_directly(fake, capture)?, request(7)?));
        }
    }
    Ok(work)
}

/// What one receiver recorded over its share of a tier.
///
/// The tier's totals are the sum of these, but the assertions are made per
/// receiver as well as in total: a receiver that quietly fell into the
/// degenerate world is cancelled out by a healthy one in the arithmetic and is
/// visible in neither the count nor the outcome.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct ReceiverRecord {
    /// How many runs this receiver was given.
    runs: usize,
    /// How many of them reported a verified publication.
    published: usize,
    /// How many reported an unobserved effect.
    unknown: usize,
    /// How many failed outright.
    failed: usize,
    /// How many creates the receiver was asked for.
    creates: usize,
    /// How many read-backs reached it.
    reads: usize,
}

impl ReceiverRecord {
    /// This receiver's share added to `total`.
    fn plus(self, total: Self) -> Self {
        Self {
            runs: total.runs.saturating_add(self.runs),
            published: total.published.saturating_add(self.published),
            unknown: total.unknown.saturating_add(self.unknown),
            failed: total.failed.saturating_add(self.failed),
            creates: total.creates.saturating_add(self.creates),
            reads: total.reads.saturating_add(self.reads),
        }
    }

    /// The create count the receiver can account for: one per run, exactly.
    ///
    /// Every run here has a fault-free scenario, so this is `==`, not `<=`. The
    /// weaker bound is what let the big tiers pass while most of their runs
    /// published nothing at all.
    fn assert_no_fault_receiver(
        self,
        receiver: usize,
        tier: usize,
        unknown_reason: &str,
    ) -> TestResult {
        let runs = self.runs;
        let creates = self.creates;
        let published = self.published;
        let reads = self.reads;
        assert_eq!(
            creates, runs,
            "tier {tier} receiver {receiver}: {runs} fault-free runs produced \
             {creates} creates — a duplicate-post defect doubles this, and a \
             lost create shows up as the shortfall"
        );
        assert_eq!(
            published, runs,
            "tier {tier} receiver {receiver}: {runs} fault-free runs produced \
             {published} verified publications and {} unknown — every run on a \
             receiver under the capture ceiling and the review ceiling must \
             verify. First reason: {unknown_reason}",
            self.unknown
        );
        assert!(
            reads >= creates,
            "tier {tier} receiver {receiver}: {reads} read-backs cannot account \
             for {creates} creates — a publication was reported without an \
             independent read"
        );
        Ok(())
    }
}

/// Read one receiver's record back from its own fixture and assert it.
///
/// `reports` is the receiver's slice of the pipeline's output in *input order*,
/// which is what [`lgwks_bot::rt::task::join_all_bounded`] guarantees, so each
/// run is counted against the receiver it was actually bound to rather than
/// against whichever one happened to finish first. The record is returned as
/// well as checked, so the tier's totals are the sum of exactly the numbers
/// that were asserted per receiver.
fn read_receiver(
    fake: &FakeGh,
    receiver: usize,
    tier: usize,
    reports: &[Report<ReviewOutcome>],
) -> Result<ReceiverRecord, Box<dyn std::error::Error>> {
    let mut record = ReceiverRecord {
        runs: reports.len(),
        ..ReceiverRecord::default()
    };
    for report in reports {
        if report.output().is_some_and(ReviewOutcome::is_published) {
            record.published = record.published.saturating_add(1);
        } else if report.output().is_some_and(ReviewOutcome::is_unknown) {
            record.unknown = record.unknown.saturating_add(1);
        } else if report.disposition().label() == "Failed" {
            record.failed = record.failed.saturating_add(1);
        }
    }
    record.creates = fake.creates()?;
    record.reads = fake.reads_of("/pulls/7/reviews")?;
    record.assert_no_fault_receiver(receiver, tier, &first_unknown_reason(reports))?;
    Ok(record)
}

/// The first unverified publication's own reason, for a failure message.
///
/// Taken from the run itself rather than reconstructed, because the whole
/// question is what a caller would have been told.
fn first_unknown_reason(reports: &[Report<ReviewOutcome>]) -> String {
    match reports
        .iter()
        .filter(|report| report.output().is_some_and(ReviewOutcome::is_unknown))
        .map(why_unknown)
        .next()
    {
        Some(reason) => reason,
        None => String::from("no run reported an unknown outcome"),
    }
}

/// Run one saturation tier, sharded across receivers, and assert each record.
///
/// The runs are genuinely concurrent — a bounded
/// [`lgwks_bot::rt::task::join_all_bounded`] pipeline, not a loop of sequential
/// `block_on` calls — because "concurrent" is the property under test and a
/// sequential loop would measure admission arithmetic rather than the
/// receiver's behaviour under load.
///
/// The tier is sharded across receivers of at most [`RUNS_PER_RECEIVER`] runs
/// each, because the fixture — not the code under test — is what does not
/// survive one receiver holding ten thousand reviews. The shard is stated in
/// full at that constant; the short form is that a merged receiver pipes a
/// quadratic read-back and drives every run past a ceiling, at which point the
/// family measured a degenerate world and passed anyway.
///
/// The observation is the receiver's own record, checked per receiver and in
/// total: with no fault configured, creates equal runs exactly (a duplicate-post
/// defect produces more), every run verifies (an unobservable publication means
/// the world went degenerate), reads are at least creates (a publication
/// reported without an independent observation produces fewer), and the owner
/// joins every run it started. One definition, because the tiered family and the
/// subject family ask this question of the *same* pipeline: a second hand-rolled
/// copy would be a second place for the receiver arithmetic to drift.
pub(super) fn run_saturation_tier(
    concurrency: usize,
    in_flight: usize,
    label: &str,
) -> Result<ReceiverRecord, Box<dyn std::error::Error>> {
    // Ceiling division: the last receiver carries the remainder, so no run is
    // dropped and no receiver carries more than the shard bound.
    let sizes = shard_sizes(concurrency);
    let receivers = sizes.len();
    let fakes: Vec<FakeGh> = (0..receivers)
        .map(|receiver| {
            let fake = FakeGh::install(label, HEAD)?;
            fake.configure(Scenario::new(HEAD).created_body(BODY))?;
            lgwks_std::trace::debug!(
                receiver = receiver,
                "saturation: receiver installed for this shard"
            );
            Ok(fake)
        })
        .collect::<Result<Vec<FakeGh>, std::io::Error>>()?;
    let host = host_named("reviewer-saturation", concurrency)?;

    let started = std::time::Instant::now();
    // Every input is built up front and owned, because a spawned task is
    // `Send + 'static`: a borrowed fixture could not cross into one, and
    // building the inputs outside the task also means a setup failure is
    // reported here rather than swallowed as a missing result.
    let job = std::sync::Arc::new(review_task_send()?);
    let work = saturation_inputs(&fakes, &sizes, CAPTURE)?;
    assert_eq!(
        work.len(),
        concurrency,
        "the sharded inputs must be the tier's own run count: one receiver over \
         would drop a run and one under would run a run nobody bound"
    );
    let host = std::sync::Arc::new(host);
    // One sample per run: the wall time from the run being polled to its report,
    // so the percentiles below include queueing behind `in_flight`, not only the
    // run's own work. The lock is taken after the await, never across it.
    let latencies = std::sync::Arc::new(std::sync::Mutex::new(Vec::with_capacity(concurrency)));
    let sink = std::sync::Arc::clone(&latencies);
    let futures = work.into_iter().map(move |(gh, request)| {
        let host = std::sync::Arc::clone(&host);
        let job = std::sync::Arc::clone(&job);
        let sink = std::sync::Arc::clone(&sink);
        async move {
            let begun = std::time::Instant::now();
            let report = host.run(&job, (gh, request)).await;
            if let Ok(mut samples) = sink.lock() {
                samples.push(begun.elapsed());
            }
            report
        }
    });
    let reports = lgwks_bot::Runtime::new()?
        .block_on(lgwks_bot::rt::task::join_all_bounded(in_flight, futures));
    let elapsed = started.elapsed();
    // A poisoned recorder means a run panicked while timing itself; that is a
    // failed tier, not an empty sample set.
    let mut samples = latencies
        .lock()
        .map_err(|poisoned| format!("a run panicked while recording its latency: {poisoned}"))?
        .clone();
    assert_eq!(
        samples.len(),
        concurrency,
        "concurrency {concurrency}: every run must record exactly one latency sample"
    );
    samples.sort_unstable();
    let at = |per_mille: usize| {
        let rank = samples.len().saturating_mul(per_mille).div_ceil(1000);
        samples[rank.saturating_sub(1).min(samples.len().saturating_sub(1))]
    };
    // `install_default` fails once a subscriber exists (the second tier in this
    // binary), which is the expected case, so the failure is a debug record.
    if let Err(error) = lgwks_std::trace::install_default("lgwks-saturation") {
        lgwks_std::trace::debug!(error = %error, "saturation: subscriber already installed");
    }
    lgwks_std::trace::info!(
        label = label,
        runs = concurrency,
        in_flight = in_flight,
        wall_ms = elapsed.as_millis(),
        p50_ms = at(500).as_millis(),
        p95_ms = at(950).as_millis(),
        p99_ms = at(990).as_millis(),
        max_ms = at(1000).as_millis(),
        "SATURATION latency per run, queueing included"
    );

    // The output is in input order and the inputs were receiver-major, so one
    // cursor walks both: no size is recomputed here and no slice can drift from
    // the receiver whose fixture is being read.
    let mut total = ReceiverRecord::default();
    let mut cursor = 0usize;
    for (receiver, (fake, shard)) in fakes.iter().zip(&sizes).enumerate() {
        let end = cursor.saturating_add(*shard);
        let record = read_receiver(fake, receiver, concurrency, &reports[cursor..end])?;
        cursor = end;
        total = record.plus(total);
    }

    assert_eq!(
        cursor, concurrency,
        "concurrency {concurrency}: the owner must join every run it started, not \
         drop the ones still in flight"
    );
    total.assert_no_fault_receiver(concurrency, concurrency, "n/a")?;

    lgwks_std::trace::info!(
        concurrency = concurrency,
        receivers = receivers,
        runs_per_receiver = RUNS_PER_RECEIVER,
        in_flight = in_flight,
        published = total.published,
        unknown = total.unknown,
        failed = total.failed,
        creates = total.creates,
        reads = total.reads,
        "saturation: the receivers' record at this concurrency"
    );
    lgwks_std::trace::info!(
        concurrency = concurrency,
        receivers = receivers,
        elapsed_ms = u64::try_from(elapsed.as_millis())?,
        "saturation: wall time for every run at this concurrency"
    );
    Ok(total)
}
