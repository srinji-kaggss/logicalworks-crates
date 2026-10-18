# lgwks_std 2.0.0 / lgwks_ast 1.1.0 / lgwks_deps 3.0.0 / lgwks_macros 1.1.1 / lgwks_bot 2.0.0 - 2026-10-06 (2/20)

Release: lgwks_std 2.0.0 / lgwks_ast 1.1.0 / lgwks_deps 3.0.0 / lgwks_macros 1.1.1 / lgwks_bot 2.0.0 - 2026-10-06. Page 2 of 20. [Changelog index](../../CHANGELOG.md)

### lgwks_bot — the supervised tree is captured, signalled by pid, and reported (#263) (continued)

- **A real `setsid` escape is stopped, and observed stopped.**
  `tests/it/process_escape.rs::a_session_escape_is_captured_and_stopped_by_pid`
  (renamed from `a_session_escape_is_not_reported_as_complete_tree_cleanup`,
  whose claim was the old group-only boundary) asserts the escapee is gone by
  `kill -0`, the receipt is `CleanupConfirmed`, and the containment report names
  a table-reading mechanism, counts the escapee and is complete (INV-BOT-112).
- **Shutdown no longer aborts a cleanup mid-drain (INV-BOT-156).** This was the
  root cause of `process_escape`'s unexplained intermittent failure. After its
  50 ms cooperative grace, `Supervisor::shutdown` aborted every task still
  running. That included process tasks that had answered the token and were
  spending capture rounds against `ps`, at p50 16 ms and max 68 ms per snapshot
  under load. Each such task was then reported `Aborted` with no receipt. That
  happened in 10 of 1,000 iterations at load 15. Shutdown now keeps
  absorbing while a spawned process task is live, bounded by
  `PROCESS_CLEANUP_GRACE` (2 s). The module passes 1,000 of 1,000 at load up to
  16.6. `shutdown_reports_every_draining_cleanup_rather_than_aborting_it` shuts
  down 32 trees at once and fails 20 of 20 runs with the bound set to zero.
- **The escape tests no longer leak.** Their sibling ran `sh -c 'echo; sleep 30'`.
  Killing the shell orphaned the `sleep`, which kept holding the test's output
  pipe. That was the "by design" `leaky` verdict the module used to explain away.
  The sibling now `exec`s its sleep.

### lgwks_std — `process`: descendant capture, per-process signals, running-set observation (#263)

The `process` feature could stop a group and could not stop a tree. These four
primitives are the other half, and every one of them is additive.

- `process::capture_descendants(root)` returns a `DescendantSet`: every process
  descended from `root` at the instant of the call, sorted, deduplicated, and
  bounded by the new `MAX_CAPTURED_DESCENDANTS` (4096) with an
  `is_truncated()` flag rather than a short answer. Linux reads each process's
  own child list from `/proc/<pid>/task/<tid>/children` — one read per process
  in the tree, not one per process on the host — and falls back to a single
  `pid`/`ppid` `ps` snapshot (`ContainmentMechanism::ProcessTableSnapshot`)
  where no such list exists; the mechanism is reported, because "every process
  the supervisor started is gone" is a claim about a mechanism and not only about
  an outcome. The snapshot is also the Linux fallback for a kernel built without
  `CONFIG_PROC_CHILDREN`.
- `process::kill_process(pid)` signals one process, for a descendant that has
  left the group: `killpg` cannot reach it and a pid can. Non-positive ids are
  refused before any signal leaves the process, exactly as the group primitives
  refuse them.
- `process::process_exists(pid)` is signal zero against one pid, so it can be
  used on a process this supervisor does not own without disturbing it.
- `process::running_processes(&[pid])` is the batch form, and the difference
  between *present* and *running* is why it exists: a process that has exited
  and awaits its reap still holds its id and answers `kill -0`, so a cleanup
  that reported such a pid as a survivor would report every shell that forked
  one child as a leak. An unreadable state for a pid the table says exists is
  reported running, because a receipt that claims less than it established is
  the honest one.

**What is not claimed.** The capture is a snapshot of the parent relation taken
while `root` is alive, so a descendant orphaned before the call has been adopted
by the platform's init and is no longer reachable through the root. On Linux
`PR_SET_CHILD_SUBREAPER` would re-parent such an orphan to *this* process, which
makes it findable and unattributable: a process never observed under this root
cannot be proved to have come from this root rather than from another supervisor
in the same process, and signalling it would break the never-signal-outside rule.
The read is therefore a tree walk and not an orphan adoption, and this is the
residual `lgwks_bot::rt::supervise` records on the receipt.

Seeded family: `tests/it/sim_descendants.rs`
(`a_seeded_sweep_agrees_with_the_model_at_every_step`,
`the_same_seed_replays_the_same_sweep_trace`,
`distinct_seeds_drive_distinct_sweeps`,
`a_seeded_descendant_is_captured_and_stopped_whatever_the_seed_draws`,
`two_trees_never_capture_each_other`,
`a_vacant_root_captures_nothing_and_names_its_mechanism`), over real forked
trees with `setsid` escapees.

### lgwks_bot — a change tick, and the sources that can report one (#279)

`Observe::revision` is a source's own answer to "has anything moved?", read on
the calling thread **before** the poll it may skip. A source that reports the
revision its committed value was read at is not polled at all: no future is
built, no value is boxed, nothing is allocated, and the observation wave is
narrower by however many sources are quiet. One vtable call and one `u64`
comparison is the whole cost of an unchanged check.

- The default is `None`, which is the value-comparison path every source took
  before and every source this crate ships still takes: a revision only pays
  when something *other than the poll* maintains it, and a source whose poll is
  how it learns everything about the world has no revision to report.
  `docs/bot-on-ecs.md` §4.1 carries the table of which shipped source takes
  which path and the reason for each.
- **A revision is committed together with its value, or not at all** — the
  fingerprint-commit rule (#99 / INV-BOT-5) with a change tick in place of a
  digest. The commit is the one arm of `observe_fold`'s second pass that put a
  new value in the observation slot, so a poll that failed, was cancelled at its
  deadline, or produced a value equal to the one already held leaves the
  revision where it was and the next tick asks again.
- **Comparison is equality only**, so a revision that wraps (`u64::MAX` → 0) or
  regresses is a change rather than an ordering a wrap could invalidate. A
  revision that moved while the value did is the one broken promise and is
  deliberately the cheap direction: a spare poll, never a suppressed effect.
- A source that declares its cached baseline unsound is polled regardless of its
  revision: the substrate has been told its held value is not what the source
  would report now, and a revision is a claim about a value rather than a repair
  of one. `TickReport::forced` and INV-BOT-120 are unchanged.
- Two defects the path exposed and this change repairs: a freshly spawned
  `Revision` reads as changed with nothing committed behind it, and a transition
  opened over no observation is never walked — so its entries stayed `NotStarted`
  and every one of them was reported pending. A transition is now opened **over
  an observation** or not at all.
- INV-BOT-155 records the rule, enforced by `tests/it/sim_change_ticks.rs`
  (seeded: both paths fire on the same ticks, wrap and regression are changes,
  a forced refresh beats a quiet revision, one movement fires once, replay).


### lgwks_bot — a tick is measured stage by stage, and the rig that measures it runs again (#279)
