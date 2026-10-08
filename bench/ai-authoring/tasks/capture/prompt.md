# Task: `capture`

Write a Rust library crate whose crate root (`src/lib.rs`) exposes exactly these
public items:

```rust
#[derive(Debug)]
pub enum CaptureError {
    Spawn(String),
    Deadline,
    Cancelled,
}

#[derive(Debug, PartialEq, Eq)]
pub struct CaptureResult {
    head: Vec<u8>,
    total_bytes: u64,
    truncated: bool,
}

impl CaptureResult {
    pub fn new(head: Vec<u8>, total_bytes: u64, truncated: bool) -> Self;
    pub fn head(&self) -> &[u8];
    pub fn total_bytes(&self) -> u64;
    pub fn truncated(&self) -> bool;
}

pub async fn solve(
    argv: Vec<String>,
    head_limit: usize,
    deadline: std::time::Duration,
) -> Result<CaptureResult, CaptureError>
```

`solve` runs `argv[0]` with `argv[1..]` as arguments and must satisfy this
contract:

- On success, `head()` is the first `head_limit` bytes of the child's standard
  output, `total_bytes()` is the exact number of bytes the child wrote to
  standard output, and `truncated()` is whether the child wrote more than
  `head_limit` bytes. Retained bytes never exceed `head_limit`, however much
  the child writes.
- If the child does not exit before `deadline`, stop the whole process group
  and resolve with `Err(CaptureError::Deadline)`.
- If the program cannot be started at all, resolve with
  `Err(CaptureError::Spawn(reason))` naming why. A failure after the child
  started is never reported as `Spawn`.
- If the caller drops the returned future mid-run, no descendant may still be
  running afterwards: children, grandchildren and anyone they forked must all
  be stopped, and none may remain as a zombie. The work the future owns must
  stop with it.
- An empty `argv` resolves with `Err(CaptureError::Spawn(..))` without
  starting anything.

You may use only the items on the API sheet that accompanies this task, plus
`std` and `ai_task_support`. Do not use any crate that is not on the sheet.

Reply with exactly one fenced ```rust block. The block must be the complete
contents of `src/lib.rs` and nothing else — no prose outside the block.
