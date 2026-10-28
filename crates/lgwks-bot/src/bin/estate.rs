//! The estate evening as a smoke: every tier, both doors, every refusal, one report.
//!
//! Run it on a built artifact to prove the assembly there. It builds each tier, runs through
//! `run_task` and `run_blocking`, cancels, drains both ways, drops an estate inside a runtime,
//! exercises every typed refusal, and prints the terminal record. Every number it prints is also
//! asserted: the first deviation records a mismatch and the exit is non-zero, so a clean exit is a
//! proved evening. `the_smoke_binary_proves_the_evening` (`tests/it/estate.rs`) runs it on every
//! test run and reads the same exit.

use std::io::Write as _;

use lgwks_bot::gate::GrantReplacement;
use lgwks_bot::rt::estate::{Estate, EstateError, EstateReport};
use lgwks_bot::rt::supervise::TaskId;
use lgwks_bot::spec::EffectScope;
use lgwks_bot::task::{Disposition, task};
use lgwks_bot::verb::{Execute, Observe};
use lgwks_bot::{Auth, Bot, BotError, Cap, EffectLifetime, GrantSet};

/// The script vocabulary on the shipped binary: what each runtime answers
/// through the public interface. A submodule rather than inline code, so the
/// smoke keeps one responsibility and the vocabulary keeps its own.
#[path = "estate/vocabulary.rs"]
mod vocabulary;

/// One line to the caller's locked stdout; a write error reaches `main` as an I/O error.
fn say(line: &str) -> std::io::Result<()> {
    writeln!(std::io::stdout().lock(), "{line}")
}

/// What one run proved, owned so the async evening can hand it back to `main`.
#[derive(Debug)]
struct RunCheck {
    /// Whether the run succeeded.
    succeeded: bool,
    /// The value it answered, when it succeeded.
    output: Option<u64>,
    /// The located failure, when it did not.
    error: Option<String>,
}

/// A source that answers once and requires `bot.net`, so the grant-replacement
/// arm below (live revocation) has an authority to withdraw mid-run.
struct SmokeSource {
    /// The one capability this source requires.
    caps: [Cap; 1],
}

impl SmokeSource {
    /// A source that requires `bot.net`.
    fn gated() -> Self {
        Self { caps: [Cap::net()] }
    }
}

impl Observe for SmokeSource {
    /// The observed value.
    type Output = u16;

    fn required_caps(&self) -> &[Cap] {
        &self.caps
    }

    async fn poll(&self, call: (Auth, ())) -> Result<u16, BotError> {
        call.0.check(self.required_caps())?;
        Ok(503)
    }

    fn domain_id(&self) -> &str {
        "smoke::source"
    }
}

/// An action that counts its invocations: the observable effect a live
/// revocation has to stop.
struct SmokeAction {
    /// How many times the action ran.
    fired: std::rc::Rc<std::cell::Cell<usize>>,
}

impl Execute for SmokeAction {
    /// The observed value it consumes.
    type Input = u16;
    /// It answers nothing.
    type Output = ();

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    fn effect_lifetime(&self) -> EffectLifetime {
        EffectLifetime::Local
    }

    async fn execute_action(&self, call: (Auth, &u16)) -> Result<(), BotError> {
        call.0.check(Execute::required_caps(self))?;
        self.fired.set(self.fired.get().saturating_add(1));
        Ok(())
    }

    fn domain_id(&self) -> &str {
        "smoke::action"
    }
}

/// The supervisor's process doors, proved on the shipped artifact rather than
/// trusted from the tests: record-then-release gating, the release handshake,
/// and every deny-builder refusal arm.
///
/// A gated lane that ran before its identity was recorded, an empty denial
/// the builders accepted, or a profile a control byte slipped into would fail
/// here rather than in a coordinator. Every line below is asserted, so a
/// door that stops working fails the smoke.
#[cfg(all(unix, feature = "process"))]
async fn process_doors(mismatch: &mut Option<String>) -> std::io::Result<()> {
    use lgwks_bot::rt::process::{Confinement, DenyBeneath, ProcessSpec, SandboxProfile};
    use lgwks_bot::rt::supervise::{GatedIdentifiedSpawn, Supervisor, TaskOutcome};
    use lgwks_bot::rt::time::{Instant, sleep};
    use std::time::Duration;

    // The deny builders refuse what they cannot confine, before any fork.
    if DenyBeneath::new().validate().is_ok() {
        record(
            mismatch,
            "an empty denial list must not validate".to_owned(),
        );
    }
    let mut many = DenyBeneath::new();
    for _ in 0..=DenyBeneath::MAX_PATHS {
        many.deny("/srv/denied");
    }
    if many.validate().is_ok() {
        record(
            mismatch,
            format!(
                "a denial past {} paths must not validate",
                DenyBeneath::MAX_PATHS
            ),
        );
    }
    let mut overlong = DenyBeneath::new();
    overlong.deny(format!("/{}", "x".repeat(DenyBeneath::MAX_PATH_BYTES)));
    if overlong.validate().is_ok() {
        record(
            mismatch,
            "a denial past the path bound must not validate".to_owned(),
        );
    }
    let mut one = DenyBeneath::new();
    one.deny("/srv/denied");
    if let Err(error) = one.validate() {
        record(
            mismatch,
            format!("one absolute denial must validate: {error}"),
        );
    }
    let Confinement::DenyBeneath(denied) = Confinement::deny_beneath(["/srv/denied"]) else {
        record(
            mismatch,
            "deny_beneath must build the deny variant".to_owned(),
        );
        return Ok(());
    };
    if denied.validate().is_err() {
        record(mismatch, "the built denial must validate".to_owned());
    }
    // The profile builders refuse what could not confine: empty sources and
    // control bytes, eagerly at construction and again at validation.
    if SandboxProfile::try_new(String::new()).is_ok() {
        record(mismatch, "an empty profile must not validate".to_owned());
    }
    if SandboxProfile::try_new("a\x01b").is_ok() {
        record(
            mismatch,
            "a profile holding a control byte must not validate".to_owned(),
        );
    }
    if let Err(error) = SandboxProfile::try_new("(version 1)(allow default)") {
        record(mismatch, format!("a plain profile must validate: {error}"));
    }
    let mut profile = SandboxProfile::allow_default();
    if let Err(error) = profile.validate() {
        record(
            mismatch,
            format!("the default profile must validate: {error}"),
        );
    }
    // A denial names the resolved directory, quoted for the profile: the
    // smoke's own scratch tree stands in for a coordinator's state.
    // A denial names the resolved directory, so the smoke mints a fresh one
    // with the estate's own entropy source rather than naming a fixed path.
    let unique = lgwks_std::random::bytes::<8>().map_err(std::io::Error::other)?;
    let hex: String = unique.iter().map(|byte| format!("{byte:02x}")).collect();
    let deny_dir = std::env::temp_dir().join(format!("lgwks-estate-smoke-deny-{hex}"));
    std::fs::create_dir_all(&deny_dir)
        .map_err(|error| std::io::Error::other(format!("the smoke denial dir failed: {error}")))?;
    let denied = deny_dir.join("denied");
    std::fs::create_dir_all(&denied)
        .map_err(|error| std::io::Error::other(format!("the smoke denied dir failed: {error}")))?;
    let denied_read = profile.deny_read_beneath(&denied);
    let _removed = std::fs::remove_dir_all(&deny_dir);
    if let Err(error) = denied_read {
        record(
            mismatch,
            format!("a real directory must be deniable: {error}"),
        );
    } else if !profile.source().contains("(deny file-read*") {
        record(
            mismatch,
            format!(
                "the denial is missing from the profile: {:?}",
                profile.source()
            ),
        );
    }
    // A gated lane holds its payload until the identity is recorded: the
    // release below stands in for the durable write, and the assertions
    // around it are the record-then-release order the coordinator keeps.
    let mut supervisor = Supervisor::new(1);
    let spec = ProcessSpec::new("/usr/bin/true");
    let mut spawned: GatedIdentifiedSpawn =
        match supervisor.spawn_process_identified_gated(&spec).await {
            Ok(spawned) => spawned,
            Err(error) => {
                record(mismatch, format!("the gated spawn was refused: {error}"));
                return Ok(());
            }
        };
    if spawned.is_released() {
        record(mismatch, "a new gate must read as unreleased".to_owned());
    }
    if spawned.leader().pid() <= 0 {
        record(
            mismatch,
            "the identity must be readable while the payload waits".to_owned(),
        );
    }
    if let Err(error) = spawned.release().await {
        record(
            mismatch,
            format!("the gate must release once the record is durable: {error}"),
        );
        return Ok(());
    }
    if !spawned.is_released() {
        record(mismatch, "a released gate must read as released".to_owned());
    }
    if let Err(error) = spawned.release().await {
        record(
            mismatch,
            format!("a second release must be a no-op, not {error}"),
        );
    }
    let task = spawned.task();
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(10))
        .ok_or_else(|| std::io::Error::other("the gate drain deadline overflowed the clock"))?;
    let outcome = loop {
        supervisor.reap();
        if let Some(outcome) = supervisor.next_report() {
            break outcome;
        }
        if Instant::now() >= deadline {
            record(mismatch, "the released payload never reported".to_owned());
            return Ok(());
        }
        sleep(Duration::from_millis(1)).await;
    };
    if !matches!(outcome, TaskOutcome::Completed { .. }) || outcome.task() != task {
        record(
            mismatch,
            format!("the released payload must complete under its own task, got {outcome:?}"),
        );
    }
    say(&format!(
        "process doors: gated lane completed under task {}",
        task.get()
    ))
    .map_err(|error| std::io::Error::other(format!("the process doors line failed: {error}")))?;
    Ok(())
}

/// Record the first deviation and keep it: the first mismatch is the root cause, and later ones
/// are its symptoms.
fn record(mismatch: &mut Option<String>, message: String) {
    if mismatch.is_none() {
        *mismatch = Some(message);
    }
}

/// The smoke's scratch directory, removed on every exit — an early `?` return included.
struct Scratch(std::path::PathBuf);

impl Drop for Scratch {
    /// Remove the directory; a refusal is logged, because a destructor has nowhere to return it.
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.0) {
            lgwks_std::trace::debug!(error = ?error, "estate-smoke: the scratch dir was not removed");
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut mismatch: Option<String> = None;
    let mut estate = Estate::supervised("acme", 8)?;
    // The process driver (ADR 0006 R2): every block below runs on it, so the binary that proves
    // the estate holds one runtime, as the estate does.
    let driver = estate
        .handle()
        .ok_or("a supervised estate carries the process driver")?;
    let supervised = estate.supervisor().is_some();
    say(&format!(
        "estate: tenant={} tier={} runtime={} supervisor={supervised} store={}",
        estate.tenant(),
        estate.tier(),
        estate.handle().is_some(),
        estate.run_store().is_some(),
    ))?;
    let probe = task("estate-probe", |_scope, input: u64| async move {
        Ok(input.saturating_add(1))
    })
    .map_err(|error| format!("the probe task is refused: {error}"))?;
    let (first, second, report): (RunCheck, RunCheck, EstateReport) = driver.block_on(async {
        let run = estate.run_task(&probe, 41).await;
        let first = RunCheck {
            succeeded: matches!(run.disposition(), Disposition::Succeeded),
            output: run.output().copied(),
            error: run.error().map(|error| error.to_string()),
        };
        let run = estate.run_task(&probe, 42).await;
        let second = RunCheck {
            succeeded: matches!(run.disposition(), Disposition::Succeeded),
            output: run.output().copied(),
            error: run.error().map(|error| error.to_string()),
        };
        estate.cancel();
        (first, second, estate.shutdown().await)
    });
    say(&format!(
        "run 41: succeeded={} output={:?} error={:?}",
        first.succeeded, first.output, first.error,
    ))?;
    say(&format!(
        "run 42: succeeded={} output={:?} error={:?}",
        second.succeeded, second.output, second.error,
    ))?;
    if !first.succeeded || first.output != Some(42u64) {
        record(
            &mut mismatch,
            format!(
                "run 41 is not a successful 42: succeeded={} output={:?} error={:?}",
                first.succeeded, first.output, first.error,
            ),
        );
    }
    if !second.succeeded || second.output != Some(43u64) {
        record(
            &mut mismatch,
            format!(
                "run 42 is not a successful 43: succeeded={} output={:?} error={:?}",
                second.succeeded, second.output, second.error,
            ),
        );
    }
    say(&format!(
        "shutdown: tenant={} tier={} admitted={} refused={} supervised={}",
        report.tenant(),
        report.tier(),
        report.runs_admitted(),
        report.runs_refused(),
        report.supervised().succeeded,
    ))?;
    if report.runs_admitted() != 2 || report.runs_refused() != 0 {
        record(
            &mut mismatch,
            format!(
                "the shutdown accounts for two admitted runs, not {} admitted and {} refused",
                report.runs_admitted(),
                report.runs_refused(),
            ),
        );
    }

    let mut estate = Estate::supervised("acme", 8)?;
    estate.cancel();
    let report = estate
        .shutdown_blocking()
        .map_err(|error| format!("the blocking drain failed: {error}"))?;
    say(&format!(
        "blocking drain: completed={} admitted={}",
        report.supervised().completed,
        report.runs_admitted(),
    ))?;
    if report.supervised().completed != 0 || report.runs_admitted() != 0 {
        record(
            &mut mismatch,
            "the empty cancel drain is not empty".to_owned(),
        );
    }

    let mut bare = Estate::in_process("acme")?;
    say(&format!(
        "in-process: runtime={} supervisor={}",
        bare.handle().is_some(),
        bare.supervisor().is_some(),
    ))?;
    let run = bare
        .run_blocking(&probe, 6)
        .map_err(|error| format!("the in-process run failed: {error}"))?;
    say(&format!(
        "in-process run: disposition={} output={:?}",
        run.disposition(),
        run.output(),
    ))?;
    if !matches!(run.disposition(), Disposition::Succeeded) || run.output() != Some(&7u64) {
        record(
            &mut mismatch,
            format!(
                "the in-process run is not a successful 7: {:?}",
                run.error()
            ),
        );
    }
    let report = bare
        .shutdown_blocking()
        .map_err(|error| format!("the in-process drain failed: {error}"))?;
    say(&format!(
        "in-process drain: tier={} admitted={}",
        report.tier(),
        report.runs_admitted(),
    ))?;
    if report.runs_admitted() != 1 {
        record(
            &mut mismatch,
            format!(
                "the in-process drain accounts for one run, not {}",
                report.runs_admitted(),
            ),
        );
    }
    // Dropping a runtime inside async is the panic tokio raises; the estate owns none, so reaching
    // the next line proves dropping it there releases its work and leaves the driver running.
    driver.block_on(async move { drop(bare) });
    say("in-process drop inside a runtime: ok")?;

    let unique = lgwks_std::random::bytes::<8>()
        .map_err(|error| format!("the scratch name failed: {error}"))?;
    let hex: String = unique.iter().map(|byte| format!("{byte:02x}")).collect();
    let dir = std::env::temp_dir().join(format!("lgwks-estate-smoke-{hex}"));
    let _scratch = Scratch(dir.clone());
    let durable = Estate::durable("acme", 8, &dir)?;
    say(&format!("durable: store={}", durable.run_store().is_some()))?;
    let run = durable
        .run_blocking(&probe, 7)
        .map_err(|error| format!("the durable run failed: {error}"))?;
    say(&format!(
        "durable run: disposition={} output={:?}",
        run.disposition(),
        run.output(),
    ))?;
    if !matches!(run.disposition(), Disposition::Succeeded) || run.output() != Some(&8u64) {
        record(
            &mut mismatch,
            format!("the durable run is not a successful 8: {:?}", run.error()),
        );
    }
    let report = driver.block_on(async { durable.shutdown().await });
    say(&format!(
        "durable drain: admitted={}",
        report.runs_admitted(),
    ))?;
    if report.runs_admitted() != 1 {
        record(
            &mut mismatch,
            format!(
                "the durable drain accounts for one run, not {}",
                report.runs_admitted(),
            ),
        );
    }

    match Estate::supervised("", 8) {
        Err(EstateError::InvalidTenant { .. }) => {
            say("refused: an empty tenant")?;
        }
        Err(error) => {
            record(
                &mut mismatch,
                format!("an empty tenant is InvalidTenant, not {error}"),
            );
        }
        Ok(_) => {
            record(&mut mismatch, "an empty tenant must not build".to_owned());
        }
    }
    match Estate::supervised("acme", 0) {
        Err(EstateError::Host { .. }) => {
            say("refused: a zero bound")?;
        }
        Err(error) => {
            record(&mut mismatch, format!("a zero bound is Host, not {error}"));
        }
        Ok(_) => {
            record(&mut mismatch, "a zero bound must not build".to_owned());
        }
    }
    match Estate::supervised("acme", usize::MAX) {
        Err(EstateError::Host { .. }) => {
            say("refused: a bound past the ceiling")?;
        }
        Err(error) => {
            record(
                &mut mismatch,
                format!("a bound past the ceiling is Host, not {error}"),
            );
        }
        Ok(_) => {
            record(
                &mut mismatch,
                "a bound past the ceiling must not build".to_owned(),
            );
        }
    }
    let blocker = dir.join("blocker");
    std::fs::write(&blocker, b"a file, not a directory")
        .map_err(|error| format!("the blocker file failed: {error}"))?;
    match Estate::durable("acme", 8, &blocker) {
        Err(EstateError::Store { .. }) => {
            say("refused: an unopenable store")?;
        }
        Err(error) => {
            record(
                &mut mismatch,
                format!("an unopenable store is Store, not {error}"),
            );
        }
        Ok(_) => {
            record(
                &mut mismatch,
                "an unopenable store must not build".to_owned(),
            );
        }
    }

    // The #703 owner pattern on the shipped artifact: place probe tasks on the
    // estate's own supervisor and reconcile the tracked set through the same
    // accessors an owner reads. Nothing here depends on scheduler timing: the
    // tracked set changes only on this thread (spawn, reap, wait_idle join),
    // so a snapshot and the accessors read beside it always agree, and
    // `wait_idle` ends every placed body before the drain counts its reports.
    // Every number below is asserted, so a lost report fails the smoke rather
    // than parking an owner.
    let mut tracked_estate = Estate::supervised("acme", 8)?;
    let mut seen: Vec<TaskId> = Vec::new();
    let tracking = driver.block_on(async {
        let supervisor = tracked_estate.supervisor()?;
        for _ in 0..3 {
            supervisor.spawn(|_| async {}).await;
        }
        // One state read three ways, with no entry point between the reads
        // that could reap: a listed task is tracked, always.
        let snapshot = supervisor.snapshot();
        if supervisor.in_flight()
            != snapshot.live().len().saturating_add(snapshot.live_truncated())
        {
            record(
                &mut mismatch,
                format!(
                    "the tracking accessors disagree with the snapshot: in_flight={} listed={} truncated={}",
                    supervisor.in_flight(),
                    snapshot.live().len(),
                    snapshot.live_truncated(),
                ),
            );
        }
        for live in snapshot.live() {
            seen.push(live.task);
            if supervisor.task_state(live.task).is_none() {
                record(
                    &mut mismatch,
                    format!("a listed task has no state: {}", live.task.get()),
                );
            }
        }
        // Every placed body ends here, never on a timer: `wait_idle` joins
        // what the spawns' own reaps have not already joined, and the roomy
        // bound keeps every report. Totals come from the supervisor's own
        // counters, so a body absorbed early still counts exactly once.
        supervisor.wait_idle().await;
        supervisor.reap();
        let mut drained = 0usize;
        while supervisor.next_report().is_some() {
            drained = drained.saturating_add(1);
        }
        let stats = supervisor.stats();
        Some((stats.spawned, stats.completed, drained, supervisor.in_flight()))
    });
    match tracking {
        Some((spawned, completed, drained, live_now)) => {
            say(&format!(
                "tracking: placed=3 spawned={spawned} completed={completed} drained={drained} live_now={live_now}"
            ))?;
            if spawned != 3 || completed != 3 {
                record(
                    &mut mismatch,
                    format!(
                        "three placed tasks did not end exactly once: spawned={spawned} completed={completed}"
                    ),
                );
            }
            if drained != 3 {
                record(
                    &mut mismatch,
                    format!("three placed probe tasks drained {drained} reports, not 3"),
                );
            }
            if live_now != 0 {
                record(
                    &mut mismatch,
                    format!("the drain left {live_now} tasks tracked"),
                );
            }
        }
        None => {
            record(
                &mut mismatch,
                "the supervised tier has no supervisor to track".to_owned(),
            );
        }
    }
    // Joins are consumptive: every task the snapshot listed above left the
    // set through a join, so each has a report and no state.
    if let Some(supervisor) = tracked_estate.supervisor() {
        for task in &seen {
            if supervisor.task_state(*task).is_some() {
                record(
                    &mut mismatch,
                    format!("a joined task still has a state: {}", task.get()),
                );
            }
        }
    }

    // Live revocation on the shipped artifact (#716): a firing bot is narrowed
    // mid-run through `replace_grants`, and its very next tick is denied
    // naming the withdrawn capability. Every number below is asserted, so a
    // grant replacement that does not take effect fails the smoke.
    let fired = std::rc::Rc::new(std::cell::Cell::new(0usize));
    let mut bot = Bot::builder("smoke-replace")
        .observe(SmokeSource::gated())
        .on(
            |value: &u16| *value >= 500,
            SmokeAction {
                fired: std::rc::Rc::clone(&fired),
            },
        )
        .with_effects(
            EffectScope::ephemeral().map_err(|error| format!("the smoke scope failed: {error}"))?,
        )
        .build(&GrantSet::empty().grant(Cap::net()))
        .map_err(|error| format!("the smoke bot was refused: {error}"))?;
    match bot.tick() {
        Ok(1) => {
            say("grant replacement: the bot fired once under its grant")?;
        }
        other => {
            record(
                &mut mismatch,
                format!("the smoke bot did not fire once under its grant: {other:?}"),
            );
        }
    }
    let report: GrantReplacement = bot.replace_grants(&GrantSet::empty());
    say(&format!(
        "grant replacement: withdrawn={:?} added={:?}",
        report.withdrawn(),
        report.added()
    ))?;
    if report.withdrawn().to_vec() != vec![Cap::net()] || !report.added().is_empty() {
        record(
            &mut mismatch,
            format!("the swap did not report exactly the withdrawn net grant: {report:?}"),
        );
    }
    match bot.tick() {
        Err(BotError::CapabilityDenied { .. }) => {
            say("grant replacement: the next tick is denied")?;
        }
        other => {
            record(
                &mut mismatch,
                format!("the tick after the swap must be denied, got {other:?}"),
            );
        }
    }
    if fired.get() != 1 {
        record(
            &mut mismatch,
            format!("the effect ran {} times, not once", fired.get()),
        );
    }

    // The supervisor's process doors on the shipped artifact (#689, #690,
    // #691): a gated lane runs only after its identity is recorded, and the
    // deny builders refuse what they cannot confine. The smoke proves the
    // assembly it ships, and these are doors of the supervisor the assembly
    // owns: a lane that ran before its record existed, or a denial the
    // builders accepted but no platform could hold, fails here rather than in
    // a coordinator.
    #[cfg(all(unix, feature = "process"))]
    driver.block_on(process_doors(&mut mismatch))?;

    // The script vocabulary on the shipped artifact (#649, #650, #651, #653,
    // #654, #657, #694): the runtime the words desugar into, exercised through
    // the public interface with every refusal asserted.
    match vocabulary::run(&driver) {
        Ok(()) => {
            say("vocabulary: every runtime answered through the public interface")?;
        }
        Err(error) => {
            record(&mut mismatch, format!("the vocabulary failed: {error}"));
        }
    }

    if let Some(mismatch) = mismatch {
        lgwks_std::trace::debug!(
            mismatch = mismatch.as_str(),
            "estate-smoke: the evening proved nothing"
        );
        return Err(mismatch.into());
    }
    Ok(())
}
