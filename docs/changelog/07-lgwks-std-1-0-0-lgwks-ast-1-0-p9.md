# lgwks_std 1.0.0 / lgwks_ast 1.0.0 / lgwks_deps 1.0.0 / lgwks_bot 1.0.0 / lgwks_macros 1.0.0 - 2026-10-04 (9/13)

Release: lgwks_std 1.0.0 / lgwks_ast 1.0.0 / lgwks_deps 1.0.0 / lgwks_bot 1.0.0 / lgwks_macros 1.0.0 - 2026-10-04. Page 9 of 13. [Changelog index](../../CHANGELOG.md)

### lgwks_bot Added (continued)

- `script::admit` (feature `script`): the one step a task body uses to admit model
  or tool output, and the fix for the defect a reviewer found on the `proposal`
  module above — it shipped with **no production caller**, so every property
  INV-BOT-90..94 state held for a boundary nothing invoked. The estate rule is
  "wired or it does not exist", so the capability lands called from the run path
  in the same change rather than deleted (#87 T26–T29).
  - It is a `script` block rather than a helper because a task body can only act on
    a `FlowError`: it enters its step, so a refusal reads `admit/plan` like every
    other located failure; it charges one **run-scoped** `Gate` — decoder, surface,
    `PlanBudget`, `RepairLedger` — so the fourth identical refusal across four
    separate `Host::run`s is a finite typed `Intervention` rather than four
    refusals a caller has to correlate; and it records each refusal through the
    run store under `<step>/refusal` *before* returning the error, so a run resumed
    on a fresh host reads back what the first run refused rather than re-deriving
    that nothing was refused.
  - `FlowError` gains two arms, `Refused { at, refusal, provenance }` and
    `Intervention { at, intervention }`. Both are typed and both carry the
    `Provenance` of the refused bytes, so no refusal reaching a `Report` is a
    string a caller must parse or an unattributable failure. Both are non-retryable:
    a payload refused for its content is refused however often it is re-read, and
    another attempt is exactly the repair an intervention refused.
  - `Gate` is shared by clone and its lock is held across the charge, the decode
    and the ledger update and **never across an `.await`**, so a fan-out can hand
    one gate to every body without the budget becoming per-body.
  - It admits a `Plan` of operation *names* and performs nothing. Not a fifth verb,
    and not an untyped plan interpreter: performing a plan's operations is still
    the caller's job through the existing verbs, and `EffectKnowledge` continues
    to report that a run performed no external effect.
- `task::RunStore::lookup`: the reader's door onto a durable step's value — the
  archived bytes committed for a step key under a run, or `None`. Without it, the
  only way to see what a previous instance recorded was to re-run the step that
  wrote it, which is why a resumed run could not read back a refusal. `Err` is a
  read failure (a run another tenant owns), never a miss (INV-BOT-7).
- `script::Readiness<T>`, a typed, generation-bound readiness fact and the wait
  that consumes it (#87 T18 / LC-09). A `Ready<T>` carries the `Generation` its
  instance was admitted under, so the four ways a readiness can say "no" are four
  different facts rather than one boolean: a signal from an older instance is
  `StaleGeneration`, one from a generation this readiness never issued is
  `UnknownGeneration`, a second release at a settled generation is `AlreadyReady`
  (the first release survives it — it is not a second release), and a duplicate
  failure is `AlreadyFailed`. `ReadinessError::released` is false for every arm,
  and `is_permanent` separates the one retryable refusal from the rest, so an
  enclosing `retry` cannot loop on a stale instance.
- `Readiness::fail` reaches a dependant that has **already been released**: it
  cancels the token of every dependant the release handed out, so a dependant
  still running learns the service is gone rather than talking to a dead
  process, and `failed_at` distinguishes that from a failure before anyone was
  released (which owes nobody a cancellation). `Readiness::shutdown` is the
  separate fact — a teardown is a stop, not a failure, and the waiting side sees
  `FlowError::Cancelled` so a routine restart does not read as an outage.
- `Generation`: a monotone, saturating counter with `FIRST`, `next` and `at`. Not
  a timestamp, because cross-host clock skew is unmeasured (INV-BOT-30), and not a
  random token, because a token can answer "the same or not" but never "older",
  which is exactly the question a restarted instance's surviving handle asks. A
  restart is a **new** readiness at a **new** generation, and that is what makes
  the old instance's handle unable to release anybody.
- `Readiness::wait(scope, limit)`: one admission, one `watch` subscription and one
  `select!`. No sleep and no poll loop, charged to the step's own budget through
  the existing `within` so it is bounded and stopped by the scope's stop, and an
  already-released readiness resolves without spending its budget — the property
  a guessed sleep cannot have. A readiness admits at most `MAX_DEPENDANTS`
  dependants, charged *before* a slot is taken, so a refused admission leaves
  capacity exactly as it was.
- `Supervisor::run_process_observed`, a stdout-line observer for a supervised
  child, and the `rt::supervise::LineObserver` alias that names it. The observer
  is called from inside the same pipe read that retains the child's output, on the
  bytes that read just observed, so an observation and a capture cannot disagree
  about what the child wrote; it fires before the capture ceiling is consulted
  and whether or not the bytes are retained, and never arms a timer. A long-lived
  service's readiness therefore comes from what the child printed — a `sh -c`
  child that prints a readiness line gates on its own output, with no timer
  anywhere in the path. `Supervisor::run_process` is that call with `None` and
  is now one method on every target, the Unix and non-Unix bodies differing
  behind it.
- More than one million task executions in flight at once on one node, measured
  (#152 §4). `tests/task_million.rs` (opt-in, `LGWKS_MILLION=1`) admits
  1,048,576 `Host::run` executions across sixteen tenant hosts, each saturated
  to its own `MAX_ADMITTED_TASKS` ceiling of 65,536, waits until every one is
  observed suspended inside its body at the same time, then releases them. The
  subjects are admitted, executing runs holding their permits — not queued
  requests or inert records. Measured on an Apple M5 Pro, release build, one
  current-thread runtime: all 1,048,576 in flight after 1.74 s; peak RSS
  6,445,662,208 bytes by `/usr/bin/time -l` (about 6.1 KiB per suspended run);
  release-to-resume p50=2.80 s p95=3.99 s p99=4.17 s, the single thread
  draining a million wakeups; every run returned its own doubled input, every
  host got every permit back and none admitted past its ceiling.
- `rt::clock`: one declared logical clock (`Clock`) and the wall-clock watchdog
  (`WallClock`) that pausing it cannot disable. `Clock::wall` follows real time
  and refuses a caller advance with a named reason; `Clock::virtual_at` is
  caller-advanceable, so retry, sleep, readiness and continuation deadlines are
  exactly drivable from a test. An advance past the declared ceiling saturates
  rather than wrapping, and a clock already at the ceiling refuses rather than
  reporting movement it did not make. `ClockSnapshot` is a **duration**, not an
  instant: what survives a restart is the remaining budget, which means the same
  thing on another host (#152).
- `rt::time::Deadline`: a deadline that names the clock governing it, carrying
  both the logical bound (`elapsed`, `remaining`, `is_exhausted`, driven by the
  declared clock) and the independent wall watchdog (`watchdog`,
  `watchdog_exceeded`, real time). Additive; no existing signature changed (#152).
- `Supervisor::snapshot` -> `SupervisorSnapshot`: a bounded, point-in-time view
  of live capacity, the declared ceiling, the supervisor's own counters, whether
  admission is closed, and the next eligible action (`NextAction::Admit`,
  `WaitForCapacity`, `Stopped`). It reads the owner's own admission and reporting
  fields, so it is never a second ledger; terminal outcomes stay in the report
  stream where the retention cap already governs them. The live listing is capped
  at the in-flight ceiling and reports how many it excluded, and every field is
  private behind an accessor. Additive (#152).
- The declared `Clock` now **governs the real run path** (#152, `INV-BOT-30`):
  `script::within`, the task `Host` (`HostBuilder::clock`, default wall) and the
  Supervisor's budget and deadline checks all read time through it. A body handed
  a caller-advanceable clock reports the same deadline refusal a real overrun
  would, with no real wait; a scope given no clock is unchanged. Additive: every
  existing signature still compiles and behaves as before.
