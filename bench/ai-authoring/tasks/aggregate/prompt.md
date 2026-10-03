# Task: `aggregate`

Write a Rust library crate whose crate root (`src/lib.rs`) exposes exactly these
two public items:

```rust
#[derive(Debug)]
pub enum SolveError {
    Fetch { id: u32 },
    Deadline,
    Cancelled,
}

pub async fn solve(
    ids: Vec<u32>,
    fetch: ai_task_support::Fetcher,
    deadline: std::time::Duration,
) -> Result<u64, SolveError>
```

`ai_task_support::Fetcher` is provided by the harness. It is
`Clone + Send + Sync + 'static`, and its one method is:

```rust
pub async fn fetch(&self, id: u32) -> Result<u64, ai_task_support::FetchError>
```

`solve` must satisfy this contract:

- At most **4** `fetch` calls may be in flight at the same time.
- On success, resolve with `Ok(sum)` where `sum` is the sum of the values every
  `fetch` returned.
- If a `fetch` fails, resolve with `Err(SolveError::Fetch { id })` naming a
  failing id, and **no new fetch may start after a failure has been observed**.
- If the overall `deadline` passes, resolve with `Err(SolveError::Deadline)` and
  start no further fetches.
- If the caller drops the returned future mid-run, no fetch may still be
  running afterwards: the work the future owns must stop with it.
- An empty `ids` resolves with `Ok(0)`.

You may use only the items on the API sheet that accompanies this task, plus
`std` and `ai_task_support`. Do not use any crate that is not on the sheet.

Reply with exactly one fenced ```rust block. The block must be the complete
contents of `src/lib.rs` and nothing else — no prose outside the block.
