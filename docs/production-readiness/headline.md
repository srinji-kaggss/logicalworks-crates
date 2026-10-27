# 1. The headline, before the method

Part of [production readiness](../production-readiness.md), §1. What moved after
2026-09-28 is summarised on that page.

**This does not currently pass its own release gate.** Issue
[#109](https://github.com/srinji-kaggss/logicalworks-crates/issues/109) held
eight invariants. Every repair landed by 2026-09-22 (#99 as #110, #107 as #117,
#108 as #116, and the five ladder rows), and #109 was closed on 2026-09-23 by
commit `7d9ad29f` (#142). That commit ran **five of the eight external
observations** in `tests/it/durable_crash_observation.rs`: the journal-ladder rows
(#100, #101, #102, #104, #106) were driven against a real file-backed journal,
killed mid-ladder with a real `SIGKILL`, restarted, and the recovered answer
asserted against the one the design names. The other three closed on their
repair and its in-process regression tests, not on an external observation:
#99 (the ECS poll path) has no real-store run and #108 (`locator_eligibility`)
has no real frame. Of #107's rows, T21 is now observed by
`tests/it/process_escape.rs`, which drives a real descendant that calls `setsid`
out of the group and asserts that the cleanup captured it while its leader lived,
stopped it by pid (`kill -0` reports it gone) and says so in a complete
containment report (INV-BOT-112,
[#263](https://github.com/srinji-kaggss/logicalworks-crates/issues/263)) — on
Unix only — and T22's `ProcessSpec` surface landed in #128. A closed issue is not a green
observation of the machine: the verdict below stands until those rows have one
and the mess rows of §4.4 close.

Stated plainly: you can ship this today for attended, inspectable automation
where a human is watching and can intervene. You cannot ship it yet for the
thing it is built for — a bot that runs for weeks and is allowed to act without
you.

**Update, 2026-09-28.** One of the three unrun rows moves. INV-BOT-15 — one
owner serializes journal writes and an ambiguous write is never reported as a
clean failure — now has a deterministic proof in
`journal::file::a_dropped_waiter_poisons_the_handle_and_a_reopen_does_not_duplicate`:
the waiter is dropped mid-write, the handle is poisoned, and a reopen replays
without a second effect. The simulation families now contribute 1,990
executable deterministic cases under Nextest, with 1,111 source-visible
`#[test]` cases under `tests/sim/` or `sim_*` files, including tenant isolation
at 5,000 stores and a reported ceiling at the concurrency tiers. The release
gate still does not pass: #99, #107 and #108 remain unrun, and no p50/p95/p99
SLO has been measured on the named VPS profile, so the verdict above is
unchanged. The next regression is still where `INVARIANTS.md` says it is.
