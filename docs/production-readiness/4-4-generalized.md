# 4.4 Generalized — works across declared inputs, including messy ones

Part of [production readiness](../production-readiness.md), §4, nine axes.

**⚠️ — the declared input space is covered. The messy one is not.**

Covered and gated: per-feature compile lanes for `lgwks_std` (7 of its 12
non-`default`/`full` features — `hash`, `pattern`, `json`, `wire`, `http`,
`online`, `fs-raw`) and three `lgwks_deps` single-feature checks (`tokio`,
`gpui`, `ml-candle`+`ml-tokenizers`); 4 `lgwks_bot` legs (no-default, `full`,
`net`+`process`+`fs`+`signal`, and `wasm32-wasip1` with `rt`) and one combined
28-grammar `lgwks_ast` invocation; plus RON flow parsing. It does **not** cover
every crate or every feature: `lgwks_deps` and `lgwks_macros` have no bare
`--no-default-features` lane, and `lgwks_std`'s remaining 5 features (`core`,
`trace`, `random`, `ron`, `process`) have no per-feature lane of their own. The
28-grammar figure is `DECLARED_GRAMMARS` in `crates/lgwks-ast/src/lib.rs`, not
a wider matrix.

**This is the axis the whole product turns on, and it is where the gap is
largest.** The reason automation is hard is not the happy path. It is that
reality delivers a stream of half-formed, mistimed, interrupted, conflicting
inputs, and a bot that handles only well-formed ones is a demo.

The state of that, honestly:

| Real-world mess | Covered? | Where |
|---|---|---|
| The request may or may not have landed before the process died | ✅ tested | `durable_dispatch`, `OutcomeUnknown` barrier |
| The journal itself cannot be read | ✅ tested | `durable_dispatch`, `#123` |
| An event returns, or a payload is equal but the event is new | ✅ tested | `durable_dispatch`, `#129` |
| A spawned process left orphans | ⚠️ partly | `process_ownership` (T19/T20); `process_escape` stops a real `setsid` descendant by pid and observes it gone (T21, INV-BOT-112); a descendant orphaned to init before the cleanup, and every non-Unix target, are still uncontained (#263) |
| A locator resolved to the wrong frame or wrong kind | ✅ tested | `locator_eligibility`, `#108` |
| Two equal payloads, distinct event ids | ✅ tested | `durable_dispatch` |
| A contradictory outcome overwriting a settled one | ✅ tested | `durable_dispatch`, refused |
| **The element is gone, not moved** | ⚠️ studied, not coded | [`frontier.md`](../frontier.md): false-heal 40.5–57.1% on removed elements; decoy and drift score ranges **overlap**, so no threshold separates them |
| An OS dialog stole focus mid-flow | ❌ | no coverage |
| A file was half-written when read | ✅ tested | `stability::read_stable_file`: two reads must agree on length, mtime and digest, an unsettled subject is `BotError::UnstableObservation` (pending, `NotDelivered`, never a change); `tests/it/stability.rs` drives a real file under a real child-process writer, `tests/it/sim_stability.rs` sweeps 1,024 seeds |
| Clock skew between two hosts | ✅ tested | `skew::SkewBound` declares how far two honest clocks may disagree; a lease expiry is a `MonoDeadline` on the granting clock judged on a foreign reading only through `lease_holds`, and `Auth::check_remote` is the cross-host form of `check` (INV-BOT-160). Generations take no bound (`epoch_holds` is exact). `tests/it/skew.rs` judges live leases from clocks at ±skew and past the bound over a real `Broker`; `tests/it/sim_skew.rs` sweeps 1,024 seeds |
| A credential expired mid-run | ✅ tested | `GrantSet::grant_expiring` + `Auth::check`: a lapsed proof is `BotError::CredentialExpired` (`Refused`, `RetryClass::Never`), an upstream 401/403/404 is `BotError::CredentialRejected` carrying a `NeedSet` repair (`cap::is_credential_status`, `GhError::repair`), wired through `PrSnapshotSource::poll` and every `gh` flow; `tests/it/credential.rs` spends a real credential's life on a real wall clock over a real file, `gh_binding.rs` and `sim_review_path.rs` drive a real `gh` child refusing the token, `tests/it/sim_credential.rs` sweeps 1,024 seeds |
| The locale changed a date or number format | ✅ tested (macOS) | every `lgwks_std` reader of human text (RFC 3339, hex, percent, JSON/RON numbers) answers the same under `C`, `de_DE`, `ar_SA`, `fa_IR` and `ja_JP` in their own time zones and refuses Arabic-Indic and Persian digits, a decimal comma and a grouping space; the one reader that leaves the process, `ps -o lstart`, is pinned to `TZ=UTC0 LC_ALL=C`. `tests/it/sim_locale.rs` sweeps 1,000 seeds in-process and re-runs the sweep and a process-start read in a child under each locale (INV-STD-LOCALE-1); removing the `ps` pin fails it. The in-process readers consult no locale, so for them the child re-run guards against a future libc call rather than discriminating one; a host without these locales (the Linux container) falls back to `C` and passes without exercising them |
| Two operators edited one record | ✅ tested | `cas::RecordStore` applies a write only when the record still holds the expected version; a late write is typed `BotError::Conflict { expected, found }` (`RetryClass::Never`), recorded in the store's bounded conflict log and read back on reopen (INV-BOT-161). `tests/it/cas.rs` races sixteen real threads on a real file and drives the `Execute` path; `tests/it/sim_cas.rs` sweeps 1,024 seeded schedules |
| The app updated and the selector no longer resolves | ❌ | no coverage in the wild |
| Accessibility tree mutated during a read | ❌ | no coverage |
| A non-idempotent effect was sent twice by a retrying proxy | ✅ tested | `idempotent::IdempotentPost` reuses one caller-generated key and payload on every attempt; the upstream deduplicates on the key and counts exactly one effect, and an upstream credential refusal on the path is the typed `NeedSet` repair (INV-BOT-162). `tests/it/retry_proxy.rs` stands a real loopback proxy forwarding every request three times in front of the adapter; `tests/it/sim_retry_proxy.rs` sweeps 1,024 seeded delivery schedules |
| The user cancelled halfway through | ⚠️ partly | `tests/it/sim_cancel_under_load.rs` stops a real `Host` with 1,000 flows in flight at a seeded point, over 1,000 seeds. Every flow reports `Succeeded`, `Cancelled` or `Refused`; the host's admitted and refused counts agree with the reports; no body outlives the call or starts after the stop; every permit returns. A run still queued at the stop is `Refused` whatever order the stop's wakes arrive in (INV-BOT-20). Before that rule the same seed did not replay. Supervised processes under a stop are `process_escape` (INV-BOT-156), at tens of processes rather than 1,000 |

The rows marked ❌ are not oversights to be fixed in an afternoon each. They are
the reason the commercial RPA vendors have large QA organisations. Closing the
Generalized axis is the bulk of the remaining work on this crate.
