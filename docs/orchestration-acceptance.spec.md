# Acceptance: simple authoring, complete orchestration

Status: **the acceptance campaign has run, at `dfc13a96` (`origin/main`,
2026-10-15); no row is accepted**. Every T01–T36 falsifier names its tests
([`acceptance/t-rows.toml`](acceptance/t-rows.toml)), and CI run
[38795282994](https://github.com/srinji-kaggss/logicalworks-crates/actions/runs/38795282994)
ran exactly those on the two supported containment backends, macOS
(`darwin-arm64`) and Linux (`linux-aarch64`, the container leg): both receipts
read 251 of 251 named tests passing and 35 of 36 rows `exercised`. The run's
conclusion is `cancelled`: its `GPUI Windows renderer` job hit the 5-minute job
cap, and every job the receipts read passed. The rows that need an independent
oracle run their children from outside (T03 `t03_non_yielding`, T14
`durable_crash_observation` with a real `SIGKILL`, T21 `process_escape`). T21 is
the one partial row: that run had no Windows receipt, and no escape from a job
is driven on Windows. The Windows tests for T19–T21 are named in
[`acceptance/t-rows-windows.toml`](acceptance/t-rows-windows.toml) and receipted
as `windows-x86_64` by the Windows job (owner [#263](https://github.com/srinji-kaggss/logicalworks-crates/issues/263)).
The record is that run's `acceptance-receipts` artifact; the table below renders
the macOS receipt. No row reads `accepted` because the map claims none; only the
map promotes a row. A passing existing workflow does not execute tests that have
not been added.

The evidence model, the notes on candidate tests, and the developer-experience
and multi-axis evaluation that would accept these rows are in
[orchestration-acceptance-evaluation.md](orchestration-acceptance-evaluation.md).

## Falsifiers through public interfaces

| ID | Contract | Scenario and required observation |
|---|---|---|
| T01 | DX-01/03/04 | A one-shot task returns a typed value with one task declaration and call; no fake observer/tick loop or manual reaping. |
| T02 | DX-04/05 | Compile-fail source/condition/action mismatch and cross-schema result binding; equivalent correct local borrowed/non-Send program compiles and runs. |
| T03 | DX-05/LC-08 | Drop local composition while suspended; no borrowed child escapes. An independently timed child process detects a non-yielding callback without claiming in-process preemption. |
| T04 | LC-02/03 | Nested `all`/`map` at concurrency one finishes; parents waiting for children do not hoard execution permits. |
| T05 | LC-02/11 | Large outputs, full stderr and slow stdout consumer stay within declared retained-byte ceilings; malformed/truncated framed output is not parsed as success. |
| T06 | LC-03 | Saturated task A cannot starve independent B; one slow source does not block unrelated ready work. Use controlled completions, not sleeps as the oracle. |
| T07 | LC-04 | A succeeds with a new fingerprint, B fails the atomic observation group, then B recovers; A's uncommitted update still executes exactly once. Test initial and subsequent changes. |
| T08 | LC-04 | Disconnect, watch overflow, stale remote key and invalidation failure force refresh rather than permanent quiet state. |
| T09 | DX-07 | Identical event payloads with different event IDs both execute; duplicate delivery of the same event does not. Latest-state mode reports intentional supersession separately. |
| T10 | DX-06/LC-01 | A required predecessor fails, is abandoned, is skipped or becomes Unknown; dependent writes never run merely because it is no longer active. |
| T11 | LC-05/06 | A Unknown -> NotApplied -> B Unknown; delayed A NotApplied and A Applied cannot alter B. Duplicate A evidence stays attributable to A. |
| T12 | LC-05/06 | Wrong tenant/run/action/payload/revision/environment/epoch evidence is refused without mutation; checked counter exhaustion never aliases an issued identity. |
| T13 | LC-06/07 | Permanent policy refusal plus repeated NotApplied does not yield unlimited retries; authorized repair is distinct and root budgets remain charged. |
| T14 | LC-05/12/13 | Crash before intent ack, after ack before dispatch, after dispatch before response, after response before receipt and during recovery. No unknown effect is blindly replayed. |
| T15 | LC-12 | Definition/input/order/schema drift during replay returns a typed incompatibility before new effects; replayed model results incur no new model request. |
| T16 | LC-13 | Old worker returns after owner-epoch takeover; its receipt cannot authorize a stale dispatch or overwrite the current owner. |
| T17 | LC-08 | Drop a scoped waiter during an effect, then drain the still-owning host; report uncertainty and cleanup separately. Explicit durable submission outlives its client by contract. |
| T18 | LC-09 | Readiness failure, duplicate readiness, stale generation and failure after ready reach the right caller/dependants; no readiness sleeps in task code. |
| T19 | LC-10 | `sh -c 'sleep 60 & exit 0'`: parent exit is not reported as complete tree cleanup while descendants remain. Also test nonzero parent exit. |
| T20 | LC-10 | Abort/drop before manager future's first poll; spawned descendants remain owned and are stopped or cleanup is explicitly unresolved. |
| T21 | LC-10 | Signalling failure, PID/group reuse and child escape attempts cannot kill unrelated processes or produce false cleanup success. Execute per supported platform/backend. |
| T22 | DX-10 | External consumer cannot execute a public process description via `spawn`, `status`, `output`, mutable engine handle or Deref escape. Exercise the sanctioned path positively. |
| T23 | DX-08 | Multiple missing permissions/adapters/inputs yield one complete presently knowable NeedSet. Repair only publication authority; prior analysis is not rerun. |
| T24 | DX-08/LC-01 | Same repair ticket delivered twice, stale repair epoch and denied repair cannot widen authority or repeat an already completed effect. |
| T25 | DX-09 | Native task and supported wire form produce the same normalized operation trace for the same inputs. Unknown operations fail before execution. |
| T26 | AI-02/03 | Malformed model output and tool-output instruction injection cannot change trusted intent, install tools or obtain credentials. Sandbox escape refusals remain observable. |
| T27 | AI-04 | Context reset preserves completed work, user corrections, Unknown effects and evidence references; truncated data never becomes a full-coverage claim. |
| T28 | AI-04/05 | Parallel workers cannot read another tenant's same-digest artifact; conflicting writes are serialized while independent reads progress. |
| T29 | AI-06 | Repeated unchanged failure reaches finite typed intervention, not infinite tool/model repairs; new evidence is recorded and does not erase root spend. |
| T30 | AI-07 | Same request key plus different payload is Conflict; a duplicate identical request reattaches; distinct request keys are not silently collapsed. |
| T31 | PR-02/03/04 | Untrusted PR, pagination, rename, unavailable diff and build-script execution produce correct subject/coverage and sandbox decisions. |
| T32 | PR-05/06 | Head advances between analysis and publication; report reviewed A/current B or refuse per declared policy, never relabel evidence as B. |
| T33 | PR-07/08/AI-08 | Provider accepts review but response is lost; reconciliation finds and verifies the exact review, or remains Unknown. No duplicate post. Test partially submitted reviews/comments. |
| T34 | PR-09/10/AI-08 | Real readback proves exact IDs/state/subject/payload; lost read permission reports unverified/blocking state while retaining applied-effect evidence. |
| T35 | LC-01/11 | Process exit zero with invalid result, model says done without evidence, and successful draft with failed publish all report their distinct true outcomes. |
| T36 | DX-11 | Progress buffer overflow loses only declared detail and increments counters; authoritative terminal/Unknown state is still retrievable. A quiet client never requires a task-owned reporting loop. |

Test implementations must use externally bounded subprocesses for hangs and
crash paths. A timeout on the executor that may be blocked is not an independent
oracle. Run process containment checks on the actual supported OS backends;
compiling a cfg branch is not runtime evidence for it.

## Per-row evidence at this revision

<!-- acceptance-table: start -->

Generated by the `acceptance-receipts` binary from [`docs/acceptance/t-rows.toml`](acceptance/t-rows.toml) and the receipt recorded for `dfc13a96` on `darwin-arm64` with features `full` at 2026-10-15T20:10:52Z, 1160.09s wall, over lgwks_bot with features `full`. The full command is in the receipt's `run` table; it is not repeated here because its `-E` alternation names every test in the map and is several kilobytes of prose nobody reads. A row is `exercised` only when a named test drove its falsifier; `present` means named tests cover part of the text and the gap column says what is not asserted. A row reads `accepted` only when the map claims it and the revision it was observed at is the head being rendered; 0 of 36 rows read `accepted` here. Re-record with `--write-table`; `--check` fails when this block disagrees with the map and the receipt.

| ID | State | Tests | Gap |
|---|---|---|---|
| T01 | exercised | 4/4 | — |
| T02 | exercised | 4/4 | — |
| T03 | exercised | 4/4 | — |
| T04 | exercised | 6/6 | — |
| T05 | exercised | 7/7 | — |
| T06 | exercised | 7/7 | — |
| T07 | exercised | 5/5 | — |
| T08 | exercised | 9/9 | — |
| T09 | exercised | 6/6 | — |
| T10 | exercised | 7/7 | — |
| T11 | exercised | 12/12 | — |
| T12 | exercised | 8/8 | — |
| T13 | exercised | 8/8 | — |
| T14 | exercised | 10/10 | — |
| T15 | exercised | 7/7 | — |
| T16 | exercised | 6/6 | — |
| T17 | exercised | 8/8 | — |
| T18 | exercised | 8/8 | — |
| T19 | exercised | 4/4 | — |
| T20 | exercised | 3/3 | — |
| T21 | present | 6/6 | PID reuse (a record naming a live pid with another start) and group reuse (a reaped leader's group id) are named and refuse to signal, and the row is receipted on macOS and Linux (the Linux container's bot-full JUnit, recorded as linux-aarch64); on Windows `t-rows-windows.toml` names the tests for a stop that reaches no process outside the job, and no escape from a job is driven there. |
| T22 | exercised | 10/10 | — |
| T23 | exercised | 9/9 | — |
| T24 | exercised | 7/7 | — |
| T25 | exercised | 6/6 | — |
| T26 | exercised | 9/9 | — |
| T27 | exercised | 5/5 | — |
| T28 | exercised | 10/10 | — |
| T29 | exercised | 4/4 | — |
| T30 | exercised | 9/9 | — |
| T31 | exercised | 14/14 | — |
| T32 | exercised | 5/5 | — |
| T33 | exercised | 9/9 | — |
| T34 | exercised | 6/6 | — |
| T35 | exercised | 4/4 | — |
| T36 | exercised | 5/5 | — |

At `dfc13a96` on `darwin-arm64`: 36 rows, 251/251 named tests passing, 35 rows exercised, 1 still partial, 1160.09s wall. Still partial: T21.

<!-- acceptance-table: end -->
