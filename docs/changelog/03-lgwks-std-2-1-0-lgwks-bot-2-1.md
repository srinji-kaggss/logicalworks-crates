# lgwks_std 2.1.0 / lgwks_bot 2.1.0 - 2026-10-06

Release: lgwks_std 2.1.0 / lgwks_bot 2.1.0 - 2026-10-06. Page 1 of 1. [Changelog index](../../CHANGELOG.md)

### Upgrading

Two minor bumps; nothing a 2.0.0 caller wrote stops compiling. `lgwks_ast`,
`lgwks_deps` and `lgwks_macros` are unchanged.

- **lgwks_std 2.1.0** — additive: `seeded` (#344) and the orphaned-group reap
  in `process` (#318). The new enums (`seeded::DrawError`,
  `process::OrphanReap`, `process::ProcessIdentityError`) are non-exhaustive.
- **lgwks_bot 2.1.0** — additive: `Supervisor::spawn_process_identified` and
  `IdentifiedSpawn` (#318), and `ResidualRisk::LeaderExited` on the
  non-exhaustive `ResidualRisk` (#347). **Behaviour change** a caller will see:
  a supervised process that exits on its own now reports
  `Containment::is_complete() == false` (and `ShutdownReport::is_clean()`
  follows), because the cleanup could not see a tree its leader left; a
  deadline or cancel kill is unchanged. Read `CleanupReceipt` for "the group is
  gone" and `Containment` for "nothing else survived".

### lgwks_std — a seeded stream every estate simulation can share (#344)

Every estate repository writing deterministic simulations carried its own
generator: logical_ci a splitmix64 whose bounded draw was a biased remainder,
this workspace a std-only copy included by path. `lgwks_std::random` is OS
entropy only, by INV-RANDOM-ONE-SOURCE.

- **lgwks_std** — `seeded::Seeded` (`core`, no dependency): `from_seed(u64)`,
  `next_u64`, an unbiased `below(bound)` and `index(len)`, both refusing an empty
  range with the typed `seeded::DrawError` without moving the stream. The stream
  is xoshiro256\*\* filled by splitmix64, pinned by reference vectors, and is
  word for word the stream `lgwks-bot/tests/sim/seed.rs` already drew, so every
  seed recorded against this workspace's simulations keeps its meaning. It is not
  randomness and must never feed a key, token or id. INV-STD-SEEDED-1.

### lgwks_bot — a supervisor killed with SIGKILL leaves a record its successor reaps by (#318)

A supervisor's guards kill its children's process groups on drop, cancel and
shutdown, and none of them runs when the supervisor's own process is killed
outright: the groups keep running with nobody left who owns them.

- **lgwks_std** — `process::ProcessIdentity` is a pid and the instant the OS
  records its process started (the boot id and start tick from `/proc` on Linux,
  `ps -o lstart` in UTC under the C locale elsewhere). It is stored as
  `<pid>/<start>` and parses back exactly or is refused with
  `ProcessIdentityError`. `process::identify_process` reads one, and
  `process::reap_orphaned_group` stops a recorded leader's group, the leader and
  its captured descendants only while the pid still names that leader, answering
  `OrphanReap::{Signalled, LeaderGone, LeaderReused}`. A pid that is gone or was
  reissued is never signalled.
- **lgwks_bot** — `Supervisor::spawn_process_identified` returns an
  `IdentifiedSpawn` (the task id and the group leader's identity, read before the
  child can be reaped); a spawn whose identity cannot be read is refused and its
  group killed. `spawn_process` does not read it and pays nothing.
  `rt::process` re-exports the record, its error, the reap and its answer.
  INV-BOT-157.

### lgwks_bot — a process that exits on its own no longer claims a tree it never saw (#347, #345)

When a supervised process exits on its own, the kernel re-parents its children
at the exit, so a capture walked from it afterwards reaches nothing. The cleanup
still read the process table — on macOS a whole-table `ps -A` spawn, about
12.5 ms per supervised process — and reported `is_complete()` for a `setsid`
descendant it could not have seen.

- **lgwks_bot** — `ResidualRisk::LeaderExited` names that limit. A cleanup that
  begins after the leader's own exit reads no table and still signals the group,
  and its `Containment` reports `ProcessGroupOnly` with the new risk, so
  `is_complete()` is `false` for it and `ShutdownReport::is_clean()` follows.
  A deadline or cancel kill, where the leader is alive at the capture, keeps its
  capture and its kill unchanged. **Behaviour change:** a caller that read
  `is_complete()` as "this one-shot process left nothing behind" now gets the
  honest `false`; the receipt (`CleanupConfirmed`) still says the group is gone.
  INV-BOT-158.
- **Measured** on macOS arm64, 2026-10-06, each side built and run alone from
  `origin/main` (18e4e204) and from this change: `measure_overhead 2000` puts
  `process_run` at p50 19.15 → 7.99 ms and p99 21.28 → 9.04 ms, peak RSS
  4.49 → 3.90 MB. logical_ci's 10,000-lane journey
  (`ten_thousand_lanes_run_once_each_and_one_failure_blocks_exactly_its_chain`)
  went from 28.69 s to 12.12 s at 16 slots.
- The drain simulation's seed draws now pass through the splitmix64 finalizer.
  The former multiply-only combination made every field a fixed offset of every
  other, so some pairs of choices were never drawn together; the named seed
  family was re-picked so every receipt arm stays reachable.
