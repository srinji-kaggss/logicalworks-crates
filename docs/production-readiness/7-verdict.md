# 7. The verdict, and the gate that would change it

Part of [production readiness](../production-readiness.md), §7.

**Today: not production-ready for unattended consequential automation.**
Production-ready for attended use where a human inspects the record and can
intervene, on the three operating systems CI covers, for workloads whose real-
world mess is one of the seven covered rows in §4.4.

To change the verdict, in the order that matters:

1. **Finish #109's eight external observations.** Five are run
   (`tests/it/durable_crash_observation.rs`): the journal-ladder rows have a real
   store, a real kill, a restart and the designed answer. #109 closed with the
   other three repaired but not externally observed: #99 needs the poll path
   under a real store and #108 a real frame. #107's T21 now has its descendant
   stopped and observed gone on Unix (#263, INV-BOT-112); it still has no
   Windows or orphan-adoption half. The difference they measure — "the bot
   survives its own machine dying" — is still open.
2. **Close the Generalized rows marked ❌ in §4.4.** Focus steal, clock skew,
   concurrent editors, selector drift, a mutating accessibility tree.
   This is the long pole and the reason RPA is hard.
3. **Cut the async facade's cost.** Done for the async path (#269, #320): §4.9
   measures it at 1.02x–1.17x raw Tokio at p50 and 0.93x–1.41x at p99, with 3.46
   allocations per task against raw Tokio's 2.02. What remains of the item is
   the synchronous tick (§4.9, #279).
4. **Multi-tenant negative tests.** §4.8.
5. **Hyperscale, on the profile the estate names.** §4.2 has the saturation
   curve, the declared knees, a million concurrent tasks, the recovery
   measurement and the 2 CPU / 2 GiB cgroup run. What it does not have is a real
   **1–2 vCPU / 1–2 GB VPS** figure: the cgroup run is one run in a VM on the
   development Mac, and every open-loop body is a timer, so the figure needs a
   machine whose cores are one or two and a CPU-bound body.
6. **Measure the authoring case on tasks that ask for it.** #87's contract is
   built and the fixed-model run says `futures` is the better surface on the
   tasks it asked (§4.1); the tasks that exercise what this crate adds are
   #270.

Six items. The first is a measurement campaign rather than a design problem,
and the second is most of the product.

---

*This document is a diagnostic, not a brochure. It should be regenerated
whenever a gate moves, and the ❌ rows should be visibly harder to leave in
place than to fix.*
