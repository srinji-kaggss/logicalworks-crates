# Task: `durable-retry`

Write a Rust library crate whose crate root (`src/lib.rs`) exposes exactly these
public items:

```rust
#[derive(Debug, PartialEq, Eq)]
pub enum RecoverError {
    Unknown,
    Deadline,
}

pub async fn solve(
    effect_dir: std::path::PathBuf,
    key: String,
    deadline: std::time::Duration,
) -> Result<String, RecoverError>
```

`solve` applies the effect identified by `key` exactly once, across any number
of calls and restarts, and must satisfy this contract:

- Each application of the effect appends exactly one line `applied:{key}\n` to
  the file `applied.log` inside `effect_dir` (created with its parent
  directories when missing). No other line is ever appended to that file.
- Before appending, check whether `applied.log` already holds a line
  `applied:{key}`. When it does, append nothing: the effect was already
  applied by an earlier attempt, possibly one that died before answering.
- After the append-or-skip, wait until the file `release` exists inside
  `effect_dir`, polling with a bounded wait, then return:
  - `Ok(format!("applied:{key}"))` when this call appended the line;
  - `Ok(format!("recovered:{key}"))` when the line was already there.
- When the `release` wait exceeds `deadline`, resolve with
  `Err(RecoverError::Deadline)` and append nothing further.
- The status is never `Unknown`: a call that cannot tell whether the effect
  landed must find out from `applied.log` rather than report
  `Err(RecoverError::Unknown)`. `Unknown` is returned never.
- If the caller drops the returned future mid-run, a later call still applies
  the effect exactly once in total: the dropped call leaves the record in a
  state the next call reconciles rather than duplicates.

The harness will `SIGKILL` the process running `solve` after it appends and
while it waits, then call `solve` again. Exactly one `applied:{key}` line and a
non-`Unknown` status is a pass; two lines, or `Unknown`, is a fail.

You may use only the items on the API sheet that accompanies this task, plus
`std` and `ai_task_support`. Do not use any crate that is not on the sheet.

Reply with exactly one fenced ```rust block. The block must be the complete
contents of `src/lib.rs` and nothing else — no prose outside the block.
