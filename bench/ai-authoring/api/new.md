# API sheet — the new surface: `lgwks_bot::task` and `lgwks_bot::script`

You may use only the items on this sheet plus `std` and `ai_task_support`.

This is the higher-level task and orchestration surface. Every item below is a
real public item of the crate. Signatures are given exactly as they are
declared.

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
use std::num::NonZeroUsize;

use lgwks_bot::script::{FlowError, Scope, Tenant, each};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let scope = Scope::root(Tenant::new("demo")?);
    let pool = Some(NonZeroUsize::new(8).ok_or("a non-zero bound")?);
    let total: u64 = lgwks_bot::rt::runtime::block_on(async move {
        let values = each(&scope, "double", pool, 0u64..100, |_step, value| async move {
            Ok::<u64, FlowError>(value * 2)
        })
        .await?;
        Ok::<u64, FlowError>(values.into_iter().sum())
    })?;
    assert_eq!(total, 9900, "0..100 doubled");
    Ok(())
}
```

## Notes

- `each` drives its bodies on the task that awaits it and owns them; dropping the
  future drops every body, so nothing outlives the call.
- `Host::run` applies the host's default deadline with `within`, under the scope
  `tenant/<task name>`, and reports the disposition, output and located error.
