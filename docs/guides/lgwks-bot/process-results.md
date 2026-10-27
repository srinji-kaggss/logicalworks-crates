# Bounded background work: process results and confinement

Pages of this guide: [tasks](background-work.md) ·
[child processes](background-processes.md) ·
[process results and confinement](process-results.md) ·
[loops, fan-out and blocking](bounded-loops.md) · [limits and tests](background-limits.md)

## `Supervisor::run_process`: the same runner, with the result in hand

`Supervisor::run_process(spec)` is `spawn_process` with the completion handed
back: it awaits the child and returns a `ProcessRun`
(`crates/lgwks-bot/src/rt/supervise/run.rs:332`). It reuses the same process-group
ownership, deadline and cleanup machinery, so a deadline stops the **group**,
and the run reports the same `CleanupReceipt` a supervised task would.

The report is data, and none of it is a verdict about the work:
`ProcessRun::status` (with `exit_code` and `signal`), `stdout` and `stderr` as
`CapturedStream`s, `deadline_fired`, and `cleanup`
(`crates/lgwks-bot/src/rt/process.rs:706`). Exit zero is reported as exit zero;
judging whether the command did what it was asked is the caller's job.

A stream whose policy is `StdioPolicy::Capture(limit)`
(`crates/lgwks-bot/src/rt/process.rs:66`) keeps its first `limit` bytes — the
ceiling is a `NonZeroUsize`, so it cannot be unbounded — keeps draining the pipe
so a child that writes past the ceiling never blocks, and reports the exact
byte total with a `truncated` flag. The retained buffer is sized once to the
ceiling, so the memory a capture can hold is bounded by the policy rather than
by what the child wrote.

## Reading a child's output as framed records

A child whose output is a sequence of length-prefixed records is read with
`CapturedStream::frames(ceiling)`, the door from a `ProcessRun`'s captured
stdout to the crate's one frame grammar. It cannot fail — a byte slice never
refuses a read — so the answer is the records plus a single `FrameRead` saying
why the pass stopped.

Four ways that ending goes, and they are different facts:

- **`Frame`** — the prefix arrived and every byte it named followed.
- **`TruncatedPrefix` / `TruncatedPayload`** — the *child* stopped writing
  mid-record. Only for a capture that is **not** `truncated`.
- **`MalformedPrefix`** — a complete prefix declared `0`, or a length past the
  reader's `ceiling`. Both name no record this grammar writes.
- **`CeilingReached`** — either the reader retained its whole `ceiling` of
  payload bytes, or a legal record declared more than the room remaining. A
  well-formed record with nowhere to go is a bound, not rot.

The case worth stating plainly: **a `truncated` capture never reports a clean
end.** Its retained bytes are a prefix *the capture* cut, so they are not the
child's whole output however they happen to end — including when they end
exactly on a record boundary, which is precisely what a naive reader reports as
a complete stream. `frames()` therefore overrides the three endings that are
*about where the bytes stop* — `EndOfStream`, `TruncatedPrefix`,
`TruncatedPayload` — with `CeilingReached { ceiling: <the capture's retained
capacity> }`, and `is_complete()` is `false` for each.

The two it leaves alone are the two the cut cannot have reached. A
`MalformedPrefix` was decided from a *whole* prefix the capture did retain, so
it is rot in the child's output however much output followed it. And a
`CeilingReached` the reader itself reached stopped the pass before the cut
mattered, so it names the reader's ceiling. Swapping either for the capture's
would be the fail-open direction: a caller looking for corruption gets a bound it
never hit, and a caller looking for its own bound is told something larger stopped
it. The two ceilings are separate facts: the reader's `ceiling` argument bounds
the payload bytes one pass keeps, and the capture's own
`StdioPolicy::Capture(limit)` bounds what was ever retained.

`read_frames(&mut reader, ceiling)` remains the fallible form for a reader that
can actually refuse. Prefer `frames()` when the bytes are already in hand.

`run_process` returns typed errors that separate the two worlds a caller acts
on differently (`crates/lgwks-bot/src/rt/process.rs:826`): `Refused` (the
supervisor was cancelled before the fork) and `NotStarted` (the platform
refused the program) both establish that nothing ran; `AfterStart` establishes
that the child did run and its outcome is unknown.

The shipped `domain::sys::Process` is written on this: `Process::new(command)`
applies a finite default capture ceiling and deadline
(`crates/lgwks-bot/src/domain/sys.rs:43`), `Execute` runs it through a
supervisor and folds the `ProcessRun` into a `ProcessState`, and the dispatch
certainty follows the same split — a pre-fork refusal is `Refused`, a post-start
failure is `Indeterminate`.

## Confining a child past its group

`ProcessSpec::confinement` takes a `Confinement` declared as data, and the
supervisor applies it through the platform's own exec wrapper, so the boundary
holds the child and every descendant, a `setsid` escapee included:

- `Confinement::SandboxProfile` (macOS) runs the child under `sandbox-exec -p`
  (`crates/lgwks-bot/src/rt/process/confine.rs:79`).
- `Confinement::Landlock` (Linux) runs it under util-linux `setpriv`, 2.40 or
  later, with a deny-by-default filesystem allowlist: `LandlockRules::allow(path,
  PathAccess::Read | ReadWrite)`, at most 64 rules
  (`crates/lgwks-bot/src/rt/process/confine.rs:291`). Grant the child's
  program and libraries (`/usr`, `/lib`, `/etc`) read-only and its work tree
  read-write; anything unnamed is refused by the kernel with `EACCES`.

Every refusal happens before the fork: an empty or malformed allowlist is
`InvalidInput`, a rule naming a missing path is `NotFound`, and a confinement
the platform has no wrapper for is `Unsupported`, each as
`ProcessRunError::NotStarted`. A confinement never degrades to an unconfined run.
