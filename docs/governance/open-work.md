# Governance §6 and §7

Part of [`GOVERNANCE.md`](../../GOVERNANCE.md), which indexes every section.

## 6. Open correctness and acceptance work

Reconciled 2026-10-08 against the 15 open issues; P0 rows and the flood and
facade rows updated 2026-10-09. "Crate done" means the
estate side shipped; "wiring open" means no consumer runs it yet, and per §1
unwired code is a wiring defect, not a closed item.

| Priority | Work | Observed gap and completion evidence |
|---|---|---|
| P0 | Main red on run 37933518033 | Three load-sensitive instruments (flood lock wait, fixed cleanup grace, median retry window). Fix in #393; done when #393 merges and main's next run is green |
| P0 | Publish verification | `crates.io` publish is manual and needs a human-held token. A green dry-run is not evidence. Reconcile the published artifacts against the 2.2.0 cut |
| P0 | Cross-OS lane reproduction | The three-OS matrix executes on hosted Actions and is the Portable evidence. A lane that did not run is `skip`, named in the receipt, never `pass` |
| P1 | Flood decision-mean bound (#375) | Quarantined from Linux container legs 2026-10-08; enforced on mac lanes. #392 judges paired medians; #393 stops charging the lock wait (mac: 10/10 inside the bound at 2× ncpu load). Return: the container leg re-measured with the new instrument, then 20 consecutive green container runs |
| P1 | Every gate under 5 minutes (#272) | PR wall 3m59s; one warm main run 240 s. Still needs five consecutive mains < 300 s, serial local < 300 s, cold build < 60 s / cold test < 90 s |
| P1 | Saturation curve + VPS profile (#269) | Latency gap closed (#320: p99 ≤ 1.25× raw Tokio, 3.45 allocs/task). Still needs the 1–2 vCPU / 1–2 GB VPS profile and the README boxes |
| P1 | Acceptance receipts (#271) | T01–T36 map + per-revision SQLite receipts landed (#374); 35/36 rows `exercised` (2026-10-09). Still needs T21 receipted per backend (a Linux receipt leg), the remaining `_tNN` renames, and the release run that renders rows `accepted` |
| P1 | Authoring frontier scale (#270) | `script!` arm + 3 guarantee tasks with negative controls landed (#374). Still needs 10 trials/cell, a third (frontier closed) model, and human authors |
| P1 | Vendor wiring (#266) | Done in worktree, uncommitted: 924/924 packages covered, offline build 2m43.9s, negative control refuses. Blocked on the §7 secrets-pattern decision |
| P1 | Descendant containment part 2 (#263) | Capture-before-kill, 4-round signalling, `CleanupSurvivors`, `process_escape` root cause all landed. Still needs Windows Job Objects (§7-gated) |
| P1 | Effect-dispatch crash exercise | #93 made dispatch durable and authorized; `durable-retry` (SIGKILL mid-effect, exactly-once count) now exists as a #270 guarantee task — wire its oracle into the suite before claiming R10 |
| P2 | Tick cost (#279) | Honest staging landed (#371). Still needs steady/poll-only ≤ 10×, churn ≤ 30×, zero steady-state allocs |
| P2 | macOS table read without spawn (#345) | Self-exit fast path landed (#349: 19.15 → 7.99 ms/process). Still needs the native read (§7-gated on `unsafe` or a new dep) |
| P2 | Walk prune + metadata (#343) | Crate side landed and verified (#356, #372). Wiring open: logical_ci still hand-rolls `read_dir` in `gates.rs`/`writes.rs`; adoption + cost check belong to that repo |
| P2 | Child containment hook (#337) | `env_clear` / `EnvDelta::Clear` landed with 160-seed sims (#358). Still needs the pre-exec hook (§7-gated on `unsafe`) |
| P2 | Cross-host lease/queue/run-state (#319) | Single-host lease, fenced queue and durable run state landed (#370). Cross-host (KEEL-SPEC S5.4) is untouched |
| P2 | Facade re-export gaps (#366, #367) | #366: `rt::net::tcp` / `rt::net::unix` split halves in #377, waiting on CI. #367: `lgwks_ast`'s `thiserror` re-export drags the grammar stack; unclaimed |
| Tracker | Nine-axis closure (#281) | Director-approved 2026-10-05 (proptest edge, vendor wiring, lint mechanism, CI lanes, bounded blocking pool). Work proceeds in tracker order |
| Standing | Dependency admission | Any new third-party edge goes through `skills/lgwks-dependency-admission/SKILL.md` and lands in `contract/APPROVED.toml` |

---

## 7. Decisions requiring the Director

| Decision | Conflicting authority or missing choice | Rule until decided |
|---|---|---|
| Publish of the PR #91 cut | Human-held crates.io token | The tag and changelog exist. Published artifacts are not claimed to match the tree |
| Merge without executed CI reproduction | A local PASS is not a CI run | Local receipts support source review only. Merge eligibility needs executed CI reproduction on the affected lanes unless the Director records a bounded exception naming its replacement evidence |
| rust-guard's REPETITION check | The Director authorised a bypass on 2026-09-27 for commit `b30a2716`, on the stated reason that the remaining findings are a relocated block and compiler-mandated shape, not duplication | The check is compared per file against the base commit and does not net a deletion against an addition, so a move reads as a copy. It needs three rules: net deletions against additions across the whole diff, exempt trait-impl boilerplate, and match a repeated body rather than a repeated line shape. Until then every real duplication is still extracted first and the bypass is the last step, recorded in the commit message |
| Does a lagging `Stored` fact still fence a replay? | The simulation found a fact that enters the fence 900 events behind the head and asked whether it should. Shipped answer is yes, and `sim_journal::stored_lags_history_but_still_fences_a_replay` now pins it | Until decided, the shipped behaviour stands and the pinning test fails loudly if it changes |
| Licence change or re-opening `lgwks_bot` | #73 closed it to outside contributions; the CLA instrument is deferred | `lgwks_bot` stays closed. The other three crates keep their licence map from #63 |
| `vendor/**` fixture exemption in the secrets pattern | #266 is done in worktree but uncommitted: the estate pre-commit secrets pattern refuses 20 staged `*.pem`/`*.pfx`/`*.key` fixtures that are bytes of the pinned `.crate` files with zero in-repo references. Omitting them breaks the offline build at checksum time | `vendor/` stays uncommitted until the hook owner / Director grants the exemption, then lands in three steps: (a) `vendor/` + `.cargo/config.toml`, (b) `gate-lanes.toml` + `ci.yml`, (c) `docs/releasing.md`. No bypass; nothing is committed |
| `unsafe` for the pre-exec containment hook and the macOS table read | CODEBOOK forbids `unsafe_code` workspace-wide; #337's Landlock/`sandbox_init` hook and #345's `proc_listallpids`/`sysctl` read need it (or a new admitted dep) | No `unsafe` until the Director rules. Stand-ins stand: `env_remove` + `sandbox-exec` wrap on macOS, `ps` snapshot with the self-exit fast path |
| Windows Job Object backend crate | No approved Windows job-object crate exists; #263 part 2 names it through an admitted storefront edge | No Windows containment until admission. Windows rows stay `present`-only |
| Merges on red PR check runs (Oct 8 sweep) | §3 needs hosted reproduction of the affected lanes; five of the seven PR runs (#368, #369, #370, #372, #374) read `failure` at merge | The failing jobs per run are recorded in the 2026-10-08 entry (2026-10-09). The merge rationale is not on record, and whether those merges stand as precedent is the Director's ruling. Until then it is flagged debt |

A required decision pauses dependent work. Audits, reproducible counterexamples,
and factual documentation may continue.
