# Migrating to `lgwks_bot` 1.0.0

1.0.0 removed the ways to start work that nobody owns. If you called any of the
items below, your crate stopped compiling, and the compiler's message does not
say what to use instead. This page does.

It was written from a measured migration: Keel (a large downstream consumer) went
from 64 compile errors to 0 with the surface below, and ran 4,035 of its unit
tests afterwards. Where that migration found a trap the compiler cannot show, the
trap is named here.

## What was removed, and what replaces it

| You used | Use instead | Why it moved |
|---|---|---|
| `rt::task::spawn(fut)` and its `JoinHandle` | an owned `JoinSet`, or a `Supervisor` | a handle you can drop is a task you can lose |
| `JoinHandle::await` for a value | `JoinSet::join_next().await` | the set owns the task, so the value comes back through the set |
| `JoinHandle::is_finished` | `JoinSet::try_join_next()` | a non-blocking check that also hands the value over |
| `rt::process::Command`, `Child` | `ProcessSpec` + `Supervisor::run_process` / `spawn_process` | the child is bounded and its whole process group is killed |
| `Child::kill` | drop the supervisor, or `Supervisor::cancel()` | `Child::kill` left a shell's grandchildren alive |
| `Command::output()` | `ProcessSpec::capture_stdout(limit)` then `run_process` | captured output has a ceiling |

## Pick by what you wanted

**"Run these concurrently and give me the values."** `JoinSet`.

```rust
use lgwks_bot::rt::task::JoinSet;

async fn doubled(inputs: Vec<u64>) -> Vec<u64> {
    let mut set = JoinSet::new();
    for input in inputs {
        set.spawn(async move { input * 2 });
    }
    let mut out = Vec::new();
    while let Some(joined) = set.join_next().await {
        // A task that panicked or was aborted is a `JoinError`, not an unwind.
        if let Ok(value) = joined {
            out.push(value);
        }
    }
    out
}
```

For a fixed list with ordered results, `join_all_bounded(limit, futures)` is
shorter. For ordered, fail-fast fan-out with your own error type, use
`script::FanOut`.

**"Run this in the background and tell me how it ended."** `Supervisor`. Its
tasks return `()`. To get a *value* out, hand the body a `oneshot` sender; a
supervisor reports how a task ended, never what it computed.

**"Run a command."** `ProcessSpec` and `run_process`:

```rust
use std::num::NonZeroUsize;
use std::time::Duration;
use lgwks_bot::rt::process::ProcessSpec;
use lgwks_bot::rt::supervise::Supervisor;

async fn version() -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut spec = ProcessSpec::new("rustc");
    spec.arg("--version")
        .capture_stdout(NonZeroUsize::new(64 * 1024).ok_or("non-zero")?)
        .deadline(Duration::from_secs(10));
    let mut supervisor = Supervisor::new(1);
    let run = supervisor.run_process(&spec).await?;
    Ok(run.stdout().bytes().to_vec())
}
```

## Traps the compiler cannot show

These compiled cleanly in Keel's first migration pass and were found only by
running the tests.

1. **A panic now has to be a `JoinError`, not an unwind.** Awaiting a body
   directly on the task that owns a channel sender means a panic in the body
   unwinds that task, drops the sender, and every waiter sees "channel closed"
   instead of the failure you meant to publish. Run the fallible body as its own
   `JoinSet` entry, so a panic comes back as a value you can classify.
2. **Do not stack-own the set that a cancelled caller must not kill.** A
   `JoinSet` dropped at an `.await` aborts its tasks, and an aborted task takes
   its captured sender with it. If a driver must outlive its first caller (a
   single-flight leader, for example), put the `Supervisor` in the shared slot,
   not in the caller's frame.
3. **Reserve under the lock, admit outside it.** `Supervisor::spawn` awaits a
   permit, so you cannot hold a `std` lock across it, but the slot that guards
   "only one evaluation" must be inserted atomically. Create and insert the slot
   while holding the lock (no `.await`), release it, and only then admit the
   driver. Releasing before inserting let two callers both see an empty map.
4. **`try_recv` on a `oneshot` consumes the value.** A "has it finished yet?"
   poll through it starves the later real read. Poll the supervisor
   (`next_report`, which is a peek at the oldest terminal outcome not yet handed
   over) or a `JoinSet` (`try_join_next`).
5. **Check what you redirect.** Mapping `Stdio::from(file)` to
   `StdioPolicy::Null` kept the build green and silently emptied `llvm-cov`'s
   output, because that tool writes its result to stdout. See the open gaps
   below.

## Gaps this migration found, and what closed them

Both were real limits of the 1.0.0 surface, not misunderstandings. They are
closed in the release after 1.0.0:

- **A stream cannot be attached to a file.** `ProcessSpec::stdout_to_file(path)`
  and `stderr_to_file(path)` create or truncate the file when the child starts. A
  path that cannot be opened refuses the start as an `io::Error`, and no child
  runs. On 1.0.0, redirect inside the command (`/bin/sh -c '<tool> > <path>'`) and
  quote the path.
- **No worker stack size.** `rt::runtime::Builder::thread_stack_size(Some(bytes))`
  sets it, up to `MAX_THREAD_STACK_SIZE` (256 MiB), and refuses more at build. On
  1.0.0 a bounded call chain deeper than the engine's 2 MiB default aborts the
  process with a stack overflow, and `RUST_MIN_STACK` is the only lever.
