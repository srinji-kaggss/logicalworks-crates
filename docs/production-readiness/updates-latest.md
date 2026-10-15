# 1. Updates, 2026-10-05 onward

Part of [production readiness](../production-readiness.md), §1, after
[the 2026-10-04 update](update-2026-10-04.md). The last update here is dated
2026-10-15 and says which earlier statements it replaces.

**Update, 2026-10-05.** The hold the journal module documentation named, an
acknowledged final frame whose length field was changed being truncated on open,
is closed by a refusal on the stored head
([#262](https://github.com/srinji-kaggss/logicalworks-crates/issues/262), §4.6).
The verdict above is unchanged, and no SLO has been measured on the named VPS
profile.

**Regenerated, 2026-10-05
([#280](https://github.com/srinji-kaggss/logicalworks-crates/issues/280)).** This
pass corrected what had gone stale rather than what had moved, each from the tree
at `977c79d4` or the GitHub API, with the command in the PR: the #109 rows (§1,
§4.6, §7 item 1) are described by how each one closed rather than as "unrun"; the
async runtime has a matched benchmark (§4.9, §7 item 3); every crate is tagged at
`1.0.0` or later (§4.5); the CI job count is 21 (§4.7); the authoring contract
was measured (§4.1, §6, §7 item 6). No ❌ row was upgraded. What still holds the
verdict at NO-GO: the hyperscale saturation curve (§4.2, #269), the real-world
mess rows (§4.4), Unix-only and `setsid`-escapable containment (§4.7, #263),
capacity isolation (§4.8, #268), and the three #109 rows with no external
observation.

**Update, 2026-10-15.** What moved since the passages above, each figure from
the file or run named. No score moved and no ❌ row was upgraded.

- **Hyperscale (#269): the VPS profile was measured, and the closed-loop latency
  target is met.** `cargo run --locked -p lgwks_deps --no-default-features --bin
  linux-container-tests -- vps` ran every `bench/async` door in a Linux container
  under a cgroup of 2 CPUs and 2 GiB, swap capped to the same, on 2026-10-11:
  947.8 s wall at host load 2.8. The facade read 1.14x–1.39x raw Tokio at p50 in
  the closed loop, a million tasks were concurrently admitted inside the cap with
  no OOM kill, and an overload at bound 64 recovered in 508.8 ms where the
  baseline did not return within 90 s
  ([`bench/async/vps.md`](../../bench/async/vps.md)). The closed-loop scenario
  table, re-measured at main `22f1d526` (#553, 15 rounds, load 6.86), reads p50
  1.17x / 1.02x / 1.06x / 1.07x and p99 1.41x / 1.02x / 1.09x / 0.93x, with 3.46
  allocations per task. The 4.49x this document used to carry was the 1 ms timer
  behind the drain, removed in #320 (`b1b1a3434`). §4.2, §4.9 and §7 items 3 and
  5 are rewritten from those figures. Still true: the cap is a cgroup in a VM on
  the development Mac and not a 1–2 vCPU machine, it is one run, and the
  open-loop bodies are timers
  ([`bench/async/saturation.md`](../../bench/async/saturation.md)).
- **The acceptance campaign (#421) ran.** CI run
  [38795282994](https://github.com/srinji-kaggss/logicalworks-crates/actions/runs/38795282994)
  at `dfc13a96` ran the named tests on the two supported containment backends.
  Both receipts, `darwin-arm64` and `linux-aarch64`, read 251 of 251 named tests
  passing and 35 of 36 rows `exercised`; T21 is `present` because containment is
  Unix-only (#263). The run's own conclusion is `cancelled`: its `GPUI Windows
  renderer` job hit the five-minute job cap, and every job the receipts read
  passed. No row reads `accepted`, because only the map promotes a row
  ([`docs/orchestration-acceptance.spec.md`](../orchestration-acceptance.spec.md)).
- **What the split did.** The file was 1,007 lines, over the 120-line limit for
  `.md` files, so it is now an index and the pieces it links, and every section
  keeps its number. Only passages stale against #269 and #421 were rewritten (the
  2026-10-04 update, §4.2, §4.9, §7 items 3 and 5). Every other sentence is
  carried over word for word and is as old as its own date: the CI job counts in
  §4.7, the tags in §4.5 and the 2026-09 measurements have not been regenerated.

Everything below is the reasoning behind the verdict in the headline.
