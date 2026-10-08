# API sheet — the `script!` language: `lgwks_bot::script!` over `task`/`script`

You may use only the items on this sheet plus `std` and `ai_task_support`.

This is the indented orchestration language. A `script!` block declares flows;
each flow expands to an `async fn` taking the tenant `Scope` first and
returning `Result<Output, FlowError>`, with every block expanding to one call
into `lgwks_bot::script`. Every item below is a real public item of the crate.
Signatures are given exactly as they are declared.

## Declaring and running a task

- `lgwks_bot::task::task<F>(name: &str, body: F) -> Result<Task<F>, FlowError>`
  where `F: Fn(Scope, I) -> Fut, Fut: Future<Output = Result<O, FlowError>>`.
  The name is 1..=128 bytes of ASCII letters, digits, `-`, `_`, `.`, `:`.
- `lgwks_bot::task::Task<F>` — `fn name(&self) -> &str`.
- `lgwks_bot::task::Host` — the installed owner of execution for one tenant.
  - `fn builder(tenant: &str) -> Result<HostBuilder, FlowError>`
  - `async fn run<I, O, F, Fut>(&self, task: &Task<F>, input: I) -> Report<O>`
    where `F: Fn(Scope, I) -> Fut, Fut: Future<Output = Result<O, FlowError>>`
  - `fn tenant(&self) -> &Tenant`, `fn limits(&self) -> &HostLimits`
  - `fn admission(&self) -> Admission<'_>`
  - `fn cancel(&self)`, `fn is_cancelled(&self) -> bool`
- `HostBuilder`
  - `fn max_concurrent_tasks(self, tasks: NonZeroUsize) -> Self`
  - `fn default_deadline(self, deadline: Duration) -> Self`
  - `fn clock(self, clock: Clock) -> Self`
  - `fn progress_capacity(self, steps: usize) -> Self`
  - `fn build(self) -> Result<Host, HostError>`
- `Report<O>`
  - `fn disposition(&self) -> Disposition`
  - `fn output(&self) -> Option<&O>`, `fn error(&self) -> Option<&FlowError>`
  - `fn result(&self) -> Result<&O, &FlowError>`
  - `fn into_result(self) -> Result<O, FlowError>`
  - `fn steps(&self) -> &[Arc<str>]`, `fn elapsed(&self) -> Duration`
- `Disposition` — `Succeeded`, `Failed`, `Cancelled`, `DeadlineExceeded`,
  `Refused`; `fn is_success(self) -> bool`.
- `HostError` — `Bound { what, value, max }`, `InsideRuntime`, `Runtime { cause }`.

## Durable runs: a store, a run identity, a resume

- `HostBuilder::run_store(self, dir: impl AsRef<std::path::Path>) -> Result<HostBuilder, StoreError>`
  — opens (or creates) a durable step-record store in `dir` and replays the
  records already there. A host built without one keeps nothing across attempts.
- `lgwks_bot::effect::RunId::from_hex(text: &str) -> Result<RunId, IdError>` —
  32 lowercase hex characters, not all zero. The same text is the same run on
  every attempt; `RunId` is `Copy`.
- `Host::resume<I, O, F, Fut>(&self, run: RunId, task: &Task<F>, input: I) -> Report<O>`
  — runs `task` under `run`: every step `run` already recorded replays its value
  without running its body, and the steps it did not record run now.
- `lgwks_bot::script::remember<T, Fut>(scope: &Scope, step: &str, body: impl FnOnce() -> Fut) -> Result<T, FlowError>`
  — runs `body` once per run and records its value durably under the step's
  path; a later attempt at the same path returns the recorded value. Integers
  and `String` are recordable.

## The language (`lgwks_bot::script!`)

Write `lgwks_bot::script! { ... }` at module level. Inside every flow `scope`
is the current `Scope`. Any other line is Rust, passed through as written.

- `[pub] flow name(inputs) [-> Output]:` — an `async fn` taking the tenant
  `Scope` first and returning `Result<Output, FlowError>`.
- `each x in xs:` — every item, as many at once as the machine sustains,
  results in input order, first failure stops the rest.
- `each x in xs, at most (limit) at once:` — the same, under a named limit.
- `within 2s:` — the block, or `TimedOut` when the deadline passes. Durations
  read like `20ms`, `200ms`, `5s`.
- `retry up to 3 times[, waiting 100ms]:` — the block again while it fails
  transiently, same key each attempt, within the run's retry budget
  (attempts in `1..=1000`).
- `together:` — each line underneath concurrently; `let x = ..` lines bind
  their result.
- `step name:` — a named scope: its own key and error location.
- `for x in xs:` — every item in turn, each in its own scope.
- `if cond:` / `else if cond:` / `else:` — as written.
- `let x = <block>:` — the block's last line becomes `x`.
- `run other(args)` — call another flow in this scope, await it, propagate
  its failure.
- `give back value` — return from the flow.
- `fail with reason` / `fail transiently with reason` — stop with a permanent
  / retryable failure.
- `.or_fail()` / `.or_retry()` (`lgwks_bot::script::ResultExt`) — turn a
  foreign error into a flow failure.
- Every script emits `ARCHITECTURE`: the flows it declares and the tree of
  blocks inside each, compiled from the same tokens.

The language refuses, at compile time: numeric concurrency bounds, attempts
outside `1..=1000`, zero durations, `unwrap` / `expect` / `panic!` /
`assert*!`, indexing and slicing, `loop` / `while` / `spawn`,
`block_on`, `thread::sleep`, `process::exit`, `unsafe`, and the rest of the
panicking and blocking vocabulary. Write the bounded block instead.

## Scoped orchestration (`lgwks_bot::script`)

- `lgwks_bot::script::Tenant::new(name: &str) -> Result<Tenant, FlowError>`.
- `lgwks_bot::script::Scope`
  - `fn root(tenant: Tenant) -> Scope`
  - `fn with_clock(tenant: Tenant, clock: Clock) -> Scope`
  - `fn enter(&self, step: &str) -> Result<Scope, FlowError>`
  - `fn path(&self) -> &str`, `fn key(&self) -> StepKey`
  - `fn token(&self) -> &CancellationToken`
  - `fn cancel(&self)`, `fn is_cancelled(&self) -> bool`
- `lgwks_bot::script::each`
  ```rust
  pub async fn each<I, T, F, Fut>(
      scope: &Scope,
      step: &str,
      limit: Option<NonZeroUsize>,
      items: I,
      body: F,
  ) -> Result<Vec<T>, FlowError>
  where
      I: IntoIterator,
      F: Fn(Scope, I::Item) -> Fut,
      Fut: Future<Output = Result<T, FlowError>>;
  ```
  Bounded and ordered: at most `limit` bodies at once, values in input order.
  Fail-fast and owning: the first error cancels the fan-out, drops every running
  body, starts no further item, and is returned located at its item's path. No
  `limit` sizes the fan-out to the machine.
- `lgwks_bot::script::within`
  ```rust
  pub async fn within<T, Fut>(
      scope: &Scope,
      step: &str,
      limit: Duration,
      body: Fut,
  ) -> Result<T, FlowError>
  where
      Fut: Future<Output = Result<T, FlowError>>;
  ```
  Race `body` against the deadline and against cancellation; on either, the body
  is dropped. Also `within_on(clock, scope, step, limit, body)`.
- `lgwks_bot::script::retry<T, F, Fut>(scope, step, attempts: NonZeroU32, backoff: Duration, body)`
  where `F: FnMut(Scope, u32) -> Fut, Fut: Future<Output = Result<T, FlowError>>`.
- `at_most(limit: usize) -> Result<NonZeroUsize, FlowError>`,
  `attempts(count: u32) -> Result<NonZeroU32, FlowError>`.
- `FlowError` — `Cancelled { at }`, `TimedOut { at, after }`,
  `Exhausted { at, attempts, last }`, `Throttled { .. }`, `Failed { at, reason }`,
  `Transient { at, reason }`, `Bot { at, source }`, `TooDeep { .. }`,
  `InvalidTenant { .. }`, `InvalidName { .. }`, `InvalidBound { .. }`;
  constructors `FlowError::failed(reason)`, `FlowError::transient(reason)`,
  `fn is_cancelled(&self) -> bool`.
- `lgwks_bot::script::ResultExt` — `or_fail()` / `or_retry()` on any `Result`.

## A bounded fan-out in this API

```rust
use lgwks_bot::script::{FlowError, Scope};

lgwks_bot::script! {
    /// Double every value and sum the results.
    flow double_sum(values: Vec<u64>) -> u64:
        let doubled = each value in values:
            value.saturating_mul(2)
        give back doubled.into_iter().sum()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    use lgwks_bot::script::Tenant;
    let scope = Scope::root(Tenant::new("demo")?);
    let total: u64 = lgwks_bot::rt::runtime::block_on(async move {
        double_sum(&scope, (0u64..100).collect()).await
    })?;
    assert_eq!(total, 9900, "0..100 doubled");
    Ok(())
}
```

## Authority (per-call capabilities)

- `lgwks_bot::cap::Cap` — `fn new(name: &str) -> Cap`. Shipped names:
  `Cap::NET`, `Cap::FS`, `Cap::SYS`, `Cap::NOTIFY`; any other string is a
  custom capability.
- `lgwks_bot::gate::GrantSet` — `fn empty() -> GrantSet`,
  `fn grant(mut self, cap: Cap) -> Self`.
- `HostBuilder::grants(self, grants: GrantSet) -> Self` — the authority runs
  are admitted against.
- `lgwks_bot::script::Scope::require(&self, caps: &[Cap]) -> Result<(), FlowError>`
  — checked at the step that reaches; the refusal carries the whole shortfall.

## Supervised processes (`process` feature)

- `lgwks_bot::rt::process::ProcessSpec`
  - `fn new(program: impl AsRef<OsStr>) -> ProcessSpec`
  - `fn arg(&mut self, arg: impl AsRef<OsStr>) -> &mut Self`
  - `fn capture_stdout(&mut self, limit: NonZeroUsize) -> &mut Self` — retain at most `limit` head bytes of standard output
  - `fn capture_stderr(&mut self, limit: NonZeroUsize) -> &mut Self`
  - `fn deadline(&mut self, deadline: Duration) -> &mut Self` — stop the whole group past the deadline
- `lgwks_bot::rt::supervise::Supervisor::run_process(&mut self, spec: &ProcessSpec) -> Result<ProcessRun, ProcessRunError>` — run to completion under the supervisor's ceiling, deadline and process-group ownership; dropping the returned future stops the whole group.
- `ProcessRun` — `fn exit_code(&self) -> Option<i32>`, `fn deadline_fired(&self) -> bool`, `fn stdout(&self) -> &CapturedStream`, `fn stderr(&self) -> &CapturedStream`.
- `CapturedStream` — `fn bytes(&self) -> &[u8]` (the retained head), `fn total_bytes(&self) -> u64` (the exact total), `fn truncated(&self) -> bool`.
- `ProcessRunError` — `Refused` (cancelled before the fork: nothing ran), `NotStarted { source }` (nothing ran), `AfterStart { source }` (ran; indeterminate).

## Notes

- There is no `join!` / `try_join!` on this surface, and none is needed: `each`
  bounds a fan-out, `together` joins a fixed set of branches, and a flow that
  must stop at the first failure uses either. See the sheet's refusals: a macro
  form that re-spelled those blocks would be a second name for the same bounds.
- `each` drives its bodies on the task that awaits it and owns them; dropping the
  future drops every body, so nothing outlives the call.
- `Host::run` applies the host's default deadline with `within`, under the scope
  `tenant/<task name>`, and reports the disposition, output and located error.
