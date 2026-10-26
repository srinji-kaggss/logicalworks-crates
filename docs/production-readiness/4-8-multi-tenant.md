# 4.8 Multi-tenant — isolated under concurrent use

Part of [production readiness](../production-readiness.md), §4, nine axes.

**⚠️ — covered at the stated boundaries, with the missing half named.**

`GrantSet` is a per-bot capability snapshot and `Auth` is a per-call proof. Two-
identity tests now exist at: the `Auth` boundary under concurrency
(`authority::a_tenant_without_the_grant_is_refused_while_the_other_is_served`,
sixteen threads and 256 rounds, refusals named, and
`a_proof_minted_for_one_tenant_does_not_cover_the_others_grant` in both
directions); the artifact store (`proposal::two_tenants_on_one_digest_stay_isolated`
and, racing, `concurrent_writers_of_one_key_commit_exactly_once_per_tenant`);
request keys (`request_key::two_tenants_never_share_a_request_run`); forced
refreshes (`observe_refresh`), the journal at scale
(`journal_scale::concurrent_tenant_appends_scale_with_isolation`), clocks
(`sim_clock_wiring`) and inspection (`inspect_wiring`). Two tenants through one
journal directory, each in its own journal file, under a mid-run kill are `sim_tenant_journal_kill`
(`kill_band_00..07`, 1,000 seeds). The kill lands between appends, tears the
in-flight frame at a seeded byte, or loses only its acknowledgment. After the
restart each tenant holds exactly its acknowledged prefix plus a landed frame,
never the other tenant's keys. The retry at the tail the controller saw lands a
torn append and is refused for a landed one, so every interrupted effect is
present exactly once. Capacity is admitted per tenant by
`Supervisor::with_tenancy` (INV-BOT-151; `tenancy::a_noisy_tenant_cannot_starve_a_quiet_one`,
`a_tenant_that_submits_ten_thousand_tasks_cannot_starve_another`,
`sim_tenancy_flood`).
A `Scope`'s tenant reaches every body under `each`, `retry` and
`FanOut::run_in` (`sim_tenancy_scope`: two tenants key the same path
differently, every retry attempt shares one tenant and key, `run_in` enters
the caller's step, clock and stop; the standalone `FanOut::run` keeps its
reserved tenant). The tenant name is a partition key the trusted host chooses,
not an authenticated identity: whoever holds the string runs as that tenant,
so isolation keeps honest tenants from colliding, not adversaries from choosing
a name (`Tenant::new` refuses the crate's reserved `fan-out` name, and the same
string is the same tenant). `each` and `FanOut` still take no permit — tenant admission
bounds who is admitted, not the CPU an admitted body burns — so a scope's
tenant reaches item scopes, not admission.

Measured on one macOS arm64 host (15 cores, shared with other work) and one
Linux arm64 container (4 vCPU, `rust:1.99.0-bookworm`), one run each:

- **Noisy neighbour** (A 10,000 slow tasks, B 100 × ~2 ms, limit 64):
  quiet p99 under the flood is 11.375 µs (macOS) / 5.5 µs (Linux) against
  3.828 ms / 3.084 ms alone — ratios 2‰ / 1‰ against the 2000‰ bound. The
  alone-side p99 in both runs is one slow baseline admission (host noise on a
  shared machine), not the flood: the medians are 583 ns alone vs 1.5 µs under
  the flood (macOS), sub-microsecond apart in absolute terms.
- **5,000 tenants provisioned** (`tenancy_scale`, every admission inside a 5 s
  progress bound, 0 refusals, every task terminal):

  | tier | macOS p50 / p99 / max | macOS peak RSS | Linux p50 / p99 / max | Linux peak RSS |
  |---|---:|---:|---:|---:|
  | 100 | 9.9 µs / 37.4 µs / 339 µs | 6,592 KiB | 7.0 µs / 120.2 µs / 1.91 ms | 9,536 KiB |
  | 1,000 | 7.9 µs / 34.1 µs / 83 µs | 8,128 KiB | 6.9 µs / 80.5 µs / 785 µs | 11,792 KiB |
  | 10,000 | 0.54 µs / 16.0 µs / 34 µs | 16,544 KiB | 0.63 µs / 10.1 µs / 707 µs | 19,060 KiB |
  | 100,000 | 0.46 µs / 14.6 µs / 148 µs | 97,744 KiB | 0.71 µs / 12.2 µs / 693 µs | 100,368 KiB |

  The 100,000 tier holds 100,000 parked tasks (5,000 tenants × 20): ~1 KiB of
  peak RSS per in-flight task on both hosts.
- **Adversarial split** (#351, #375). The gate is deterministic:
  `sim_tenancy_flood` drives the shipped round in virtual time, one neighbour
  caller against one to 256 attacker callers flooding 1, 4, 32 or 256
  fail-at-once submissions per neighbour task, with seeded abandonment, over
  256 seeds. With both ceilings inside half the pool the neighbour's last task
  ends at the **same virtual instant** with and without the flood — exact, not
  within 10% — and with overlapping ceilings some seed's flood does delay it
  (the negative control). The round's own step count (`DeficitRoundRobin::work`:
  one per decision, per grant pass, and per waiter a skip or compaction looks
  at) stays within four steps per event at every flood size; compacting on every
  abandonment instead of when abandoned waiters outnumber the live ones reads
  11,317 steps for 1,604 events and fails it. The wall-clock half is a report,
  not a gate: `bench/async -- --flood-split --rounds=41` (paired arms, POOL 64,
  ceiling 32, 2,000 neighbour tasks, flood 4, alternating order) read the
  neighbour's flooded throughput at p50 1018‰ / 986‰ (p99 1167‰ / 1237‰) of
  its arm alone and its own decision time at p50 1053‰ / 1010‰ (p99 1407‰ /
  1244‰), two runs on a quiet macOS host, 0.70 s wall and 4.2 MB peak RSS. The
  wall-clock tests it replaces read the host: on CI shared by thirteen lanes
  their paired ratios spread from 170‰ to 34,985‰ in one run, and the residual
  on a quiet host is a fixed 3–8 ns cache-coherence delta per decision (the
  flood's completions write the round and the pool counter from other cores), so
  a cheaper decision raised the ratio rather than lowering it (branch
  `perf/admit-outside-lock-375`, 50 → 40 ns, not landed). The Linux container
  leg runs the deterministic family like every other test.

Two tenants cannot share one journal *file*: an open takes the file's exclusive
advisory lock, so a second open of a held file is `JournalError::Locked` before a
byte is read or repaired, the holder's history is unchanged, and the path opens
again once the holder closes it
(`refusals::a_second_tenant_opening_a_held_journal_is_refused_and_nothing_changes`,
#641). The lock binds writers on one host that go through `FileJournal::open`.

*Not covered:* the kill is a cut of the file a real writer produced, not a
`SIGKILL` of two tenants' process (`durable_crash_observation` kills a single
journal for real). The 11.2% neighbour gap was seen only on GitHub's x86_64
runners and is still unreproduced anywhere else: arm64 Linux (container and
dedicated cores) and arm64 macOS show no gap, and the split above attributes
the measured difference to scheduling. An x86_64 4-vCPU host with the split
instrumentation is the run that would confirm or refute the SMT hypothesis.
