# lgwks_std 2.0.0 / lgwks_ast 1.1.0 / lgwks_deps 3.0.0 / lgwks_macros 1.1.1 / lgwks_bot 2.0.0 - 2026-10-06 (1/20)

Release: lgwks_std 2.0.0 / lgwks_ast 1.1.0 / lgwks_deps 3.0.0 / lgwks_macros 1.1.1 / lgwks_bot 2.0.0 - 2026-10-06. Page 1 of 20. [Changelog index](../../CHANGELOG.md)

### Upgrading

Three crates take a major bump; each breaking change is stated in its section below.

- **lgwks_std 2.0.0** — `wire::FormatDescriptor::pointer_width_bits` and
  `archived_u32_alignment` are `usize` (they were `u8`). Code that reads them as
  `u8` stops compiling; widen the binding.
- **lgwks_deps 3.0.0** — the lock, scan and vendor report records keep their
  fields private behind accessors (`name()`, `version()`, `checksum()`,
  `snippet()`, `missing()`, `is_complete()`), so a report cannot be edited after
  the gate produced it. Replace field reads with the accessor of the same name.
- **lgwks_bot 2.0.0** — `EffectKey::new` is removed; build a key with
  `EffectIdentity::new(run, environment, flow).key(action, attempt, digest, epoch)`.
  `TaskOutcome::cleanup()` and `ProcessRun::cleanup()` return a reference and
  `CleanupReceipt` is no longer `Copy` (#263). `Need::chain()` returns
  `Option<usize>`, and a `gh` 401/403/404 is `CredentialRejected` rather than a
  retried `DomainError` (#278 row 3).
- **lgwks_ast 1.1.0** — additive: `try_parse_within`, `DEFAULT_PARSE_DEADLINE`,
  `MAX_AST_DEPTH`, `MAX_MARKDOWN_CONTAINERS_PER_LINE`, `markdown_containers`,
  and the typed refusals on the non-exhaustive `ParseError` (#277).
- **lgwks_macros 1.1.1** — no public change; internal fixes from the nine-axis sweep.

### lgwks_bot — a tenanted admission joins a bounded backlog (#268)

`spawn_for` and `run_process_for` on a tenanted supervisor joined every finished
task before admitting, twice. A tenant whose bodies end at once therefore put the
joins of its whole flood on the critical path of whichever neighbour admitted
next: on a GitHub x86 runner the neighbour's admissions ran 14.7% slower beside a
fail-fast flood (draft PR #340, CI run 37470645014), past #268's 10% row. An
admission now joins at most `REAP_PER_ADMISSION = 2` finished tasks, once, after
the round has decided, so a queued caller reaps while its grant is on the way.
The same probe then measured 0.25% (draft PR #341, CI run 37470654341). Two per
call is still a drain: every admission places one task and joins two, so the
retained set stays at the in-flight ceiling. `Supervisor::reap`,
`Supervisor::wait_idle` and shutdown still join everything. Pinned by
`rt::supervise::tests::a_tenanted_admission_joins_a_bounded_backlog_and_keeps_the_set_at_the_ceiling`
(10,000 admissions; an unbounded reap fails it with 32 joins in one call).

### lgwks_bot — the supervised tree is captured, signalled by pid, and reported (#263)

**After the reap the cleanup observes, it does not capture.** The post-reap pass
used to walk the tree from the reaped leader once more. The leader's id is free
by then, so the walk could name another process's children, and on Linux a
reaped root has no `/proc/<pid>/task`, so every supervised process paid a
whole-table `ps` spawn: `sim_review_path`'s 10,000-run tier went from 62 s on
main to 181 s on CI (run 37468321247), and to 269 s locally. The pass now asks
only about the pids the pinned phase proved running, and reads no table when
there are none (local tier: 146 s).

A group kill reaches the members that exist when it is sent. A descendant that
called `setsid` has left the group by construction, so this cleanup now names
every process descended from the leader, signals each one by pid, and reports
which mechanism ran and which pids it could not prove gone.

- **`Containment` on every report.** `TaskOutcome::containment()` and
  `ProcessRun::containment()` report the mechanism
  (`ContainmentMechanism::ProcessTableSnapshot` here, `ProcChildrenTree` on
  Linux), how many descendants the capture named, how many a signal reached,
  which of them were still running when the drain ended, and — as
  `ResidualRisk::{TableUnreadable, CaptureTruncated}` — the limit of the
  mechanism when there is one. A consumer no longer has to walk the process
  table itself to answer "what did you stop, and what did you miss".
- **`CleanupReceipt::CleanupSurvivors { survivors }`.** Its own arm: "the group is
  gone but a `setsid` descendant of it is still running" and "the group itself is
  still there" are different facts with different repairs. The pids are named and
  never truncated.
- **Breaking, and the reason:** `TaskOutcome::cleanup()` and `ProcessRun::cleanup()`
  return a reference, and `CleanupReceipt` is no longer `Copy`. The receipt now
  carries a pid list, and a by-value read would copy it per call.
  `TaskOutcome::{Completed, Cancelled, Failed}` also gained a `containment` field
  (private; read it through the accessor). `ShutdownReport::is_clean()` now
  requires a complete containment report as well as a complete receipt.
- **The 64-yield retry is gone**, replaced by `CONTAINMENT_ROUNDS = 4`
  capture-and-signal rounds. Each round re-reads the tree, re-signals the group,
  signals every captured pid, and observes which are still *running*; the bound is
  on rounds of evidence rather than on how long a scheduler took. The drop-time
  fallback keeps a bounded `GROUP_SIGNAL_ATTEMPTS = 4` repeat for the one window a
  capture cannot reach — a member the leader forks after the capture and before it
  dies — and signals never follow the reap, so no id is signalled once the OS may
  reissue it (INV-BOT-12).
- **Present is not running.** An observation distinguishes a process that has not
  exited from one that has and awaits its reap, so a child that ended is not
  reported as a survivor, and the post-reap pass can clear a survivor the platform
  has since reaped.

**Not claimed.** The capture is a snapshot of the parent relation taken while the
leader is alive, so a descendant forked *after* the last capture and left in the
same window, and a descendant orphaned before the cleanup ran, are not signalled.
Linux `PR_SET_CHILD_SUBREAPER` would re-parent the second class here, which makes
it findable and unattributable at the same time — a process never observed under
this root cannot be proved to have come from it rather than from another supervisor
in the same process — so the read is a tree walk and not an orphan adoption. Both
limits are named on the receipt through `Containment::residual_risk()` and
`Containment::is_complete()` rather than left to be inferred from a green test.

Seeded family in `rt::supervise::tests`
(`sim_a_seeded_drain_reaches_the_models_receipt`,
`sim_the_same_seed_replays_the_same_drain_trace`,
`sim_distinct_seeds_drive_distinct_drains`,
`sim_every_receipt_arm_is_reachable_in_the_family`) drives the real drain against a
scripted capture, signaller and observer over twelve named seeds, checking every
receipt arm, every containment count and the post-reap settlement against a model.
