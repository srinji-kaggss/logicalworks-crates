//! The primitives more than one mode calls: lossless count conversions, the
//! body both sides run, the drain helpers, the tally the fairness gate compares,
//! and the print/refusal helpers every table shares.
//!
//! Moved from `main.rs` for #639; the behaviour it exercised is unchanged.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use lgwks_bot::rt::supervise::Supervisor;
use lgwks_std::trace::info;
// Every count this rig converts between `usize` and `u64` is lossless on the 64-bit
// hosts it is built for, and this assertion makes any other width a compile error
// rather than a silent clamp hidden at a conversion site.
const _: () = assert!(
    usize::BITS == u64::BITS,
    "the rig's count conversions assume a 64-bit usize"
);

/// A count as the `u64` every tally and receipt carries. Lossless: see the
/// assertion above.
pub(crate) const fn widen(count: usize) -> u64 {
    count as u64
}

/// A `u64` count as a `usize` length. Lossless: see the assertion above.
pub(crate) const fn narrow(count: u64) -> usize {
    count as usize
}

/// A duration in whole nanoseconds, saturating at `u64::MAX` (584 years) rather
/// than wrapping.
pub(crate) fn nanos(duration: Duration) -> u64 {
    duration
        .as_secs()
        .saturating_mul(1_000_000_000)
        .saturating_add(u64::from(duration.subsec_nanos()))
}

/// The bound `--overload` runs at when the caller names none.
pub(crate) const OVERLOAD_DEFAULT_BOUND: usize = 1_024;

/// How long the overload run's recovery phase lasts.
#[derive(Clone, Copy, Debug)]
pub(crate) enum RecoveryLength {
    /// As long as the baseline phase, which keeps the short form short.
    SameAsBaseline,
    /// A length the caller named, for a run that must outlast a long queue.
    Seconds(u64),
}

impl RecoveryLength {
    /// The phase length in seconds, given the baseline phase's.
    pub(crate) const fn seconds(self, baseline: u64) -> u64 {
        match self {
            Self::SameAsBaseline => baseline,
            Self::Seconds(named) => named,
        }
    }
}

// ── The two sides ───────────────────────────────────────────────────────────

/// The outcome tally both sides must agree on, exactly.
///
/// Every field is a count of something observable. The fairness gate compares
/// these field by field between the facade and the baseline; a difference in any
/// one of them means the two sides did different work and the timing is void.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Tally {
    /// Tasks the owner placed, whether or not they have ended.
    pub(crate) placed: u64,
    /// Tasks that ended by delivering a value.
    pub(crate) completed: u64,
    /// Tasks whose body observed the token and returned because of it.
    pub(crate) cancelled: u64,
    /// Tasks the runtime dropped before they finished.
    pub(crate) aborted: u64,
    /// Spawn attempts refused because every permit was taken.
    pub(crate) refused: u64,
    /// The sum of every task's own completion count, so "the body did its work N
    /// times" is compared and not just "a task ended".
    pub(crate) work_units: u64,
}

/// One unit of the body both sides run: a counter bump and a short await.
///
/// The await is what makes this an async measurement rather than a synchronous
/// one. `yield_now` is chosen over a timer so the comparison does not become a
/// measurement of the timer wheel; both sides use the same body.
pub(crate) async fn body_unit(counter: Arc<AtomicU64>) {
    counter.fetch_add(1, Ordering::SeqCst);
    tokio::task::yield_now().await;
}

// ── CPU-bound bodies ─────────────────────────────────────────────────────────

/// What an open-loop arrival's body burns.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum BodyKind {
    /// A counter bump, a yield, and a timer park: the scheduler, not the cores,
    /// is the binding constraint.
    Timer,
    /// A counter bump, a yield, and a fixed, seeded number of hash rounds: the
    /// cores are the binding constraint.
    Cpu,
}

/// Every CPU body hashes from this seed: fixed, so the facade and the baseline
/// burn identical streams and the fairness gate's work-count check still holds.
/// Varied per-task streams would be varied load, which is a different
/// measurement; matched work is the point of the rig.
pub(crate) const CPU_BODY_SEED: u64 = 0xC941_0AD5_EED4_9EED;

/// One CPU body's work: `rounds` splitmix64 rounds from `seed`.
///
/// Splitmix64's gamma, not a crate: this rig admits no new published-crate
/// edge for its own load shape, and a mixer of a few integer multiplies is
/// the cheapest deterministic CPU work with a data dependency per round (a
/// loop the compiler can prove empty is not load). The returned state must be
/// spent through [`std::hint::black_box`] by the caller: an unused pure value
/// is dead code the optimizer removes, and a benchmark that measured the
/// optimizer's mercy would be measuring nothing.
pub(crate) fn cpu_hash_rounds(seed: u64, rounds: u64) -> u64 {
    let mut state = seed;
    for _ in 0..rounds {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        state = z ^ (z >> 31);
    }
    state
}

/// Nanoseconds one CPU hash round costs on this host: the minimum of three
/// 200,000-round runs, floored at a tenth of a nanosecond.
///
/// The minimum, not the mean: calibration runs on a host whose scheduler may
/// preempt it, and a preempted sample is a sample of the scheduler, not of the
/// mixer. The floor keeps a zero-cost reading from declaring unbounded
/// capacity, which is the number this rig exists not to publish. Wall-clocked,
/// so it is provenance for the offered rates, never for the fairness gate:
/// both sides of a point run the same rounds whatever it reads.
pub(crate) fn calibrate_cpu_ns_per_round() -> f64 {
    const CALIBRATION_ROUNDS: u64 = 200_000;
    let mut best = f64::MAX;
    for _ in 0..3 {
        let start = std::time::Instant::now();
        std::hint::black_box(cpu_hash_rounds(CPU_BODY_SEED, CALIBRATION_ROUNDS));
        let nanos = start.elapsed().as_nanos() as f64;
        best = best.min(nanos / CALIBRATION_ROUNDS as f64);
    }
    best.max(0.1)
}

/// Whole microseconds `rounds` cost at `ns_per_round` each, rounded up, at
/// least one: the declared cost of a round-pinned CPU body, so the ladder
/// derives its capacity from what the bodies actually burn.
pub(crate) fn cpu_body_micros(rounds: u64, ns_per_round: f64) -> u64 {
    ((rounds as f64 * ns_per_round.max(0.1)) / 1_000.0)
        .ceil()
        .max(1.0) as u64
}

/// Parse `--body-kind`: `timer` (the scheduler binds) or `cpu` (the cores do).
pub(crate) fn parse_body_kind(text: &str) -> Result<BodyKind, String> {
    match text {
        "timer" => Ok(BodyKind::Timer),
        "cpu" => Ok(BodyKind::Cpu),
        other => Err(format!(
            "body-kind must be timer or cpu, not {other:?}: a mistyped kind that silently ran the timer door would report scheduler numbers as CPU numbers"
        )),
    }
}

/// How a facade run ends: drain everything, or stop early and report anyway.
///
/// The drain policy is the only thing that separates an honest facade run from
/// the mutant, and it is a parameter rather than a copied function so the two
/// cannot drift on anything *else* — the spawn loop, the tally, the body and the
/// ceiling are one piece of code, and the negative control differs from the
/// honest run in exactly one named decision.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Drain {
    /// Reap until every placed task has reached a terminal state.
    Complete,
    /// Reap whatever is ready at this instant and stop. The defect.
    GiveUp,
}

/// Minimal cancellation token for the baseline, built on the engine's `watch`
/// channel.
///
/// The baseline cannot use `lgwks_bot`'s own token — that would not be a raw
/// comparison — and it cannot use `tokio-util` without a second third-party
/// edge, which this workspace does not admit. `watch` is the primitive the
/// estate's own `rt::cancel` is built on, so the baseline is built on the same
/// primitive the facade uses and the comparison stays about the *facade*, not
/// about two different cancellation implementations.
pub(crate) mod tokio_util_cancel {
    use std::sync::Arc;
    use tokio::sync::watch;

    /// A parent/child token with the propagation the facade's has.
    #[derive(Clone, Debug)]
    pub struct Token {
        sender: Arc<watch::Sender<bool>>,
        receiver: watch::Receiver<bool>,
    }

    impl Token {
        /// A fresh uncancelled token.
        #[must_use]
        pub fn new() -> Self {
            let (sender, receiver) = watch::channel(false);
            Self {
                sender: Arc::new(sender),
                receiver,
            }
        }

        /// A token cancelled when this one is.
        #[must_use]
        pub fn child(&self) -> Self {
            Self {
                sender: Arc::clone(&self.sender),
                receiver: self.receiver.clone(),
            }
        }
    }

    /// Mint a fresh cancellation token, unrelated to any other, so one benchmark lane's cancel never reaches another lane.
    #[must_use]
    pub fn token() -> Token {
        Token::new()
    }
}

/// Refuse a mode that recorded failures, naming every one of them.
///
/// One refusal for every mode that accumulates a failure list rather than one per mode.
/// The shape was identical three times over in the original — build the message, log the
/// typed error, hand it back — and a third copy is where the next mode's version drifts
/// from this one without anyone noticing, because a drift in a refusal path is invisible
/// until the refusal is the thing a reader is relying on.
pub(crate) fn refuse_failed(
    tool: &str,
    what: &str,
    failures: &[String],
) -> Box<dyn std::error::Error> {
    let refusal: Box<dyn std::error::Error> =
        format!("{tool}: {} {what}: {}", failures.len(), failures.join("; ")).into();
    // The message *is* the error and there is no cause beneath it, so the log carries the
    // rendered form rather than a `.err()` on a `Box<dyn Error>` that is already the answer.
    lgwks_std::trace::debug!(error = %refusal, "refuse_failed: refusing a mode that recorded failures");
    refusal
}

/// How many completed tasks a supervisor must reach before a drain returns.
pub(crate) fn completed_target(total: usize) -> u64 {
    widen(total)
}

/// Join every task until `target` of them have reached a terminal state.
///
/// One drain for every path that drives one supervisor, because three copies of a poll loop
/// is three places for the wait quantum to drift and the quantum is a number the published
/// table is sensitive to.
///
/// The wait is [`Supervisor::wait_idle`], the facade's awaiting join, so both sides of every
/// comparison now wait on a task-completion wakeup: the baseline through `join_next`, the
/// facade through the same `JoinSet` wakeup inside `wait_idle`. Before that method existed
/// the facade could only be drained by polling `reap` on a timer, and the runtime rounds a
/// 50 us sleep up to its 1 ms tick — a fixed 1.3-1.6 ms added to every facade run, measured
/// as 4.5x raw Tokio on `quiet-async-bot` (#269). That cost was the API's, not the rig's,
/// and it is fixed in the API. The loop stays because a drain can be asked for a target the
/// placed tasks have not yet reached; it waits again rather than spinning.
pub(crate) async fn drain_supervisor(supervisor: &mut Supervisor, target: u64) {
    while supervisor.stats().completed < target {
        if supervisor.wait_idle().await == 0 && supervisor.stats().completed < target {
            // Nothing in flight and the target unmet: the target counts tasks that were
            // never placed, so no wait can reach it. Stop rather than spin.
            break;
        }
    }
}

/// Join until both supervisors have reached `target`, polling whichever is behind.
///
/// Two supervisors at once, so this cannot be [`drain_supervisor`] twice: the point of the
/// two-tenant row is that neither tenant waits on the other's drain, and a loop that drained
/// one to completion before looking at the second would measure a serial run and call it two
/// tenants.
pub(crate) async fn drain_two_supervisors(
    first: &mut Supervisor,
    second: &mut Supervisor,
    target: u64,
) {
    // Joined concurrently, so neither tenant's drain waits behind the other's.
    tokio::join!(
        drain_supervisor(first, target),
        drain_supervisor(second, target)
    );
}

/// This host's one-minute load average, and where it was read from.
///
/// Read once per run and recorded beside every figure, because a timing measured on a
/// loaded host is a real measurement of the wrong thing. On the reference host a scenario
/// table taken with three other builds running on it reported the **baseline** at 1.08 ms
/// where it reported 0.35 ms on an idle one — a 3x shift in the control leg, which moves
/// both sides of a paired ratio and is invisible without the load printed beside it.
///
/// A missing figure is absent, never zero: a host that cannot report its load reports
/// nothing about it rather than reporting an idle one.
pub(crate) fn host_load_average() -> (Option<String>, &'static str) {
    #[cfg(target_os = "linux")]
    {
        let read = std::fs::read_to_string("/proc/loadavg").ok();
        let average = read
            .as_deref()
            .and_then(|line| line.split_whitespace().next())
            .map(str::to_string);
        match average {
            Some(average) => (Some(average), "/proc/loadavg"),
            None => (None, "/proc/loadavg unreadable"),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        // `sysctl` rather than a `getloadavg` FFI edge: the first is a system utility this
        // instrument may run, the second is a C edge no crate here may author.
        // `/usr/sbin`, not `/usr/bin`: on this host `sysctl` is in `/usr/sbin`, and a
        // measurement instrument that reports "load average unavailable" because it looked
        // in the wrong directory is worse than one that never claimed to read it.
        //
        // The braces are stripped rather than worked around by picking a field: macOS
        // prints `{ 1.23 2.34 3.45 }`, so the first whitespace-separated token is `{` and
        // reading it as a number would publish a brace. Linux's `/proc/loadavg` has no
        // braces, which is why the two arms are separate rather than one parser with a
        // flag.
        let sample = std::process::Command::new("/usr/sbin/sysctl")
            .args(["-n", "vm.loadavg"])
            .output();
        let read = match sample {
            Ok(output) if output.status.success() => String::from_utf8_lossy(&output.stdout)
                .split(|character: char| character.is_whitespace() || character == '{')
                .find(|token| token.parse::<f64>().is_ok())
                .map(str::to_string),
            _ => None,
        };
        match read {
            Some(average) => (Some(average), "/usr/sbin/sysctl -n vm.loadavg"),
            None => (None, "/usr/sbin/sysctl -n vm.loadavg unreadable"),
        }
    }
}

/// Print one row of a table.
///
/// One printer for every table in the rig: the two-space indent and the cell
/// separator are decided here, so a row assembled anywhere cannot drift them.
/// The column widths themselves belong to each caller's format string, because
/// only the caller knows what its cells contain.
pub(crate) fn print_row(cells: &[String]) {
    info!("  {}", cells.join("  "));
}

/// Print a table's column names.
pub(crate) fn print_columns(columns: &[&str]) {
    print_row(
        &columns
            .iter()
            .map(|name| (*name).to_string())
            .collect::<Vec<_>>(),
    );
}

/// A latency in nanoseconds, rendered in microseconds to one decimal.
///
/// One renderer for every latency the rig prints, so no table reports the same
/// measurement in two scales and lets a reader compare them by eye.
pub(crate) fn micros(nanos: u64) -> String {
    format!("{:.1}", nanos as f64 / 1_000.0)
}

/// The `runtime_workers` and `host_cores_visible` cells every results file carries.
///
/// One derivation for every record, so two files cannot spell an absent worker count
/// differently or read the host's parallelism by a different rule.
pub(crate) fn worker_and_core_cells(workers: Option<usize>) -> (String, usize) {
    let workers = match workers {
        Some(count) => count.to_string(),
        None => "null".to_string(),
    };
    let cores = match std::thread::available_parallelism() {
        Ok(cores) => cores.get(),
        Err(_) => 0,
    };
    (workers, cores)
}

/// The CPU door's body, checked beside the code it burns with (#639).
#[cfg(test)]
mod cpu_body_tests {
    use super::{
        BodyKind, CPU_BODY_SEED, calibrate_cpu_ns_per_round, cpu_body_micros, cpu_hash_rounds,
        parse_body_kind,
    };

    #[test]
    fn sim_a_cpu_body_is_a_pure_function_of_its_seed_and_rounds() {
        // The matched-workload premise of the CPU door: the facade and the
        // baseline burn identical streams, so the same (seed, rounds) must hash
        // identically on every call, and a different seed or one more round
        // must hash differently — otherwise the two sides would not be doing
        // the same work, or two workloads would be one.
        for rounds in [1_u64, 7, 1_000, 100_000] {
            let once = cpu_hash_rounds(CPU_BODY_SEED, rounds);
            assert_eq!(
                once,
                cpu_hash_rounds(CPU_BODY_SEED, rounds),
                "{rounds} rounds did not replay: the body is not a function of its seed alone"
            );
            assert_ne!(
                once,
                cpu_hash_rounds(CPU_BODY_SEED.wrapping_add(1), rounds),
                "{rounds} rounds ignored their seed: two workloads hash as one"
            );
            assert_ne!(
                once,
                cpu_hash_rounds(CPU_BODY_SEED, rounds.saturating_add(1)),
                "{rounds} rounds ignored their length: two workloads hash as one"
            );
        }
    }

    #[test]
    fn sim_a_pinned_round_count_declares_its_calibrated_cost() {
        // The CPU door pins its rounds, so the declared body cost is the
        // rounds' calibrated cost: the rounding is upwards, so the ladder never
        // offers past what the workers sustain while the record claims
        // otherwise.
        assert_eq!(
            cpu_body_micros(1_000_000, 2.0),
            2_000,
            "1 M rounds at 2 ns/round declare 2 ms, exactly"
        );
        assert_eq!(
            cpu_body_micros(1_000_001, 2.0),
            2_001,
            "a part-microsecond rounds up, never down"
        );
        assert_eq!(
            cpu_body_micros(1, 0.1),
            1,
            "a single round still declares a body, never an empty one"
        );
    }

    #[test]
    fn sim_the_cpu_calibration_is_a_positive_finite_cost() {
        // Wall-clocked, so the assertion is loose on purpose: a round costs
        // nanoseconds, and anything outside a tenth of a nanosecond to a
        // millisecond is the scheduler in the sample, not the mixer. The sweep
        // floors the same way; this pins the floor.
        let ns = calibrate_cpu_ns_per_round();
        assert!(
            (0.1..=1_000_000.0).contains(&ns),
            "calibration read {ns} ns/round: not a cost a mixer can have"
        );
    }

    #[test]
    fn sim_body_kind_parses_strictly() {
        // A mistyped kind that silently ran the timer door would report
        // scheduler numbers as CPU numbers, so anything but the two names is a
        // refusal.
        assert_eq!(
            parse_body_kind("timer"),
            Ok(BodyKind::Timer),
            "timer must parse"
        );
        assert_eq!(parse_body_kind("cpu"), Ok(BodyKind::Cpu), "cpu must parse");
        for text in ["", "CPU", "timer ", "sleep", "timer,cpu"] {
            assert!(
                parse_body_kind(text).is_err(),
                "{text:?} parsed as a body kind: the door would run under the wrong name"
            );
        }
    }
}
