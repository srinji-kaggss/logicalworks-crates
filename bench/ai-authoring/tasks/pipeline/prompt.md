# Task: `pipeline`

Write a Rust library crate whose crate root (`src/lib.rs`) exposes exactly these
two public items:

```rust
#[derive(Debug)]
pub enum PipelineError {
    Stage { name: ai_task_support::StageName },
    Deadline,
    Cancelled,
}

pub async fn solve(
    stage: ai_task_support::Stage,
    deadline: std::time::Duration,
) -> Result<ai_task_support::Published, PipelineError>
```

`ai_task_support::Stage` is provided by the harness. It is
`Clone + Send + Sync + 'static`, and its one method is:

```rust
pub async fn run(
    &self,
    name: ai_task_support::StageName,
) -> Result<ai_task_support::Artifact, ai_task_support::StageError>
```

`ai_task_support::StageName` is the enum `StageName::FetchA`, `StageName::FetchB`,
`StageName::Combine`, `StageName::Publish`. An `Artifact` has
`fn value(&self) -> u64`; `Published` is `From<Artifact>` and also has
`fn value(&self) -> u64`.

The pipeline is: `fetch_a` and `fetch_b` are independent and may run
concurrently; `combine` requires both `a` and `b` to have succeeded; `publish`
requires `combine`.

`solve` must satisfy this contract:

- On success, resolve with `Ok(published)` carrying the artifact `publish`
  returned.
- If `fetch_a` or `fetch_b` fails, `combine` and `publish` must not run, and the
  error must name the failed stage: `Err(PipelineError::Stage { name })`.
- If any stage runs past the overall `deadline`, resolve with
  `Err(PipelineError::Deadline)` and run no later stage.
- If the caller drops the returned future mid-run, no stage may still be
  running afterwards: the work the future owns must stop with it.

You may use only the items on the API sheet that accompanies this task, plus
`std` and `ai_task_support`. Do not use any crate that is not on the sheet.

Reply with exactly one fenced ```rust block. The block must be the complete
contents of `src/lib.rs` and nothing else — no prose outside the block.
