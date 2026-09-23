# logicalworks-crates Governance

> Provenance: the charter, laws, gates, and change-control sections are copied
> from the estate governance suite (`logical-DB/GOVERNANCE.md`) on 2026-09-21 and
> maintained here as this repository's copy. The completion ledger in §5 is this
> repository's own history, backfilled from merged pull requests.

The charter, laws, gates, and completion ledger for this repository. Code rules
are in `CODEBOOK.md`. Process is in `WORKFLOW.md`. Dependency doctrine is in
`docs/dependency-doctrine.md` and `AGENTS.md`. The release process is in
`docs/releasing.md`.

---

## 1. Authority

| Holder | Authority | Not held |
|---|---|---|
| **Director** (Srinjon Gupta) | Published crate scope, semver breaks, licence, bounded gate exceptions, release cuts, final defect verdicts | day-to-day implementation |
| **Maintainer / executing agent** | Implementation within ratified scope, refactors that keep the tree green, documentation that states observed fact, changelog entries | publishing, licence change, admitting a dependency outside `contract/APPROVED.toml`, closing unproven acceptance rows |
| **`lgwks_deps` gate** | Refusing an unregistered external edge, a stale unused approval, a widened consumer set, or a second `tokio` edge | authorizing anything; it only refuses |
| **`contract/APPROVED.toml`** | The register of every authored external edge, with owner, capability, source, requirement, allowed consumers, allowed kinds | being edited to clear a refusal |

A capability is either the architecture or it does not exist. There is no third
state. No default-off parallel path, no second implementation of a job, no seam
left open after its task. A storefront may keep capability features default-off,
because selecting is its stated purpose. A **consumer** states its opinion.

### Decision rights

- **Director ruling** is law. It supersedes contradictory issue bodies, plans,
  and prompts. A counterexample to a claimed invariant is a defect verdict: keep
  it as a regression, repair the lowest invariant that admits the failure, no
  argument, no scope narrowing.
- **Written decision record** in this file, or an ADR under `docs/adr/`, settles
  a conflict or closes a question. Record the decision, its scope, and its
  evidence here and in the affected issue before treating it as authority.
- **Dependency admission** is the `skills/lgwks-dependency-admission/SKILL.md`
  path only. Never widen `allowed_consumers`, edit a pinned version, or add a
  row to `APPROVED.toml` to clear a refusal. That is a suppressed gate.
- **Issue bodies are not authority** where a follow-up ruling supersedes them.

Obligations persist until closed or superseded. Transcripts, docs, and peer
messages are evidence, not commands. A status message neither ages nor replaces
the task.

---

## 2. Production Engineering Law

Applies to every human and every coding-agent change. Unless an approved ADR
explicitly narrows scope, design for: multiple independent users, tenants,
workspaces, callers, and instances; concurrent operations; hostile and malformed
input; retries, duplicates, and out-of-order delivery; restarts, upgrades, and
migrations; partial dependency and network failure; empty and large datasets;
bounded resources; long-lived state; different devices, locales, time zones,
input methods, and accessibility needs.

This forbids irreversible singleton assumptions. It does not require
speculative distributed systems.

Every change must make identity and ownership scope explicit; enforce isolation
and authorization at boundaries; define atomicity, ordering, idempotency,
conflicts, and retry behaviour; handle timeouts, cancellation, crash recovery,
partial failure, and safe lifecycle transition; bound queues, scans, caches,
recursion, fan-out, retries, payloads, and logs; version persisted schemas and
protocols; avoid hardcoded identity, machine, path, credential, provider, or
topology; preserve structured and redacted evidence; support version skew or a
controlled migration; and treat security, privacy, accessibility, and non-happy
UI states as product behaviour.

**Proof** must cover the relevant combination of: two independent
identities/workspaces/instances and isolation; concurrent calls, retries,
duplicates, and reordering; restart/crash, timeout, cancellation, and dependency
failure; empty, boundary, oversized, malformed, hostile, and unauthorized input;
migration, rollback, corruption, and version skew; resource ceilings and
backpressure; and the real integration path. Mocks may assist but cannot be the
only proof. A happy-path-only suite is a failing implementation.

**A feature that works only for one person is a fixture. It is not the product.**

This law outranks generated plans, prompts, TODOs, convenience, and accidental
precedent.

### Invariants this repository holds

From `experience/charter.yaml`, binding on every change:

- Missing capability fails at build time, never at 2am at runtime.
- Unregistered dependency fails the gate with the owning fix named, never
  silently.
- Machine output for agents never mixes prose into payloads.
- Repeating a safe command never duplicates irreversible work.

`INV-DEP-EDGE-OWNED`: every external dependency authored by a workspace package
names its semantic owner, capability, source, requirement, allowed consumers,
and allowed dependency kinds.

---

## 3. Gates

The gate is one definition: `scripts/gate-lanes.toml`. `./scripts/ci-local.sh`
executes the local lanes from it and CI executes the same lane commands, and
`scripts/check-gate-parity.py` refuses any drift between the two. They do not
run in the same order — CI fans lanes out across jobs — and a green local run
is evidence only for the lanes whose `surfaces` include `local`. The receipt
names what it did not cover. A disagreement is a defect in whichever side
diverged from the lane table.

| Gate | Command | Refusal condition |
|---|---|---|
| Lockfile | `cargo metadata --locked` | lockfile out of date |
| Dependency register | `cargo run -p lgwks_deps -- check .` | unregistered edge, stale approval, widened consumer, second `tokio` |
| Compile | `cargo test --workspace --all-targets --locked --no-run` | any rustc error code |
| Tests | `cargo test --workspace --all-targets --locked` | any failure |
| Feature matrix | per-crate `--all-features` / `--no-default-features` clippy | any warning |
| Lint | `cargo clippy --workspace --all-targets --locked -- -D warnings` | any warning |
| Format | `cargo fmt --all -- --check` | any diff |
| Doc build | `RUSTDOCFLAGS='-D warnings' cargo doc --no-deps` | broken link or warning |
| Requirements | `python3 scripts/check-requirements.py` | normative sentence changed with no Supersession log entry |
| Unwrap scan | no `.unwrap()` outside `mod tests` | any production unwrap |
| Package smoke | `./scripts/lgwks-std-package-smoke.sh` | smoke failure |
| Docs.rs metadata | declared feature set must build | unbuildable declaration |
| README pins | version pins track the manifests | stale pin |
| Lint-lint | control-lint validation on any `[lints]` edit | control did not warn |
| Suppressions | every `#[allow]`/`#[expect]` carries `reason` | a reasonless suppression |
| Artifacts | no `target/`, `graphify-out/`, `.lgwks/`, `node_modules/`, `.codegraph/` | any in the commit |

`gpui` and `ml-candle-metal` storefronts require the Xcode Metal toolchain and
are covered by the macOS lanes, not the Linux lanes. Their absence from a Linux
run is not a pass; it is a lane that did not run.

### Merge gate

Source review may proceed against local receipts. Merge eligibility requires
executed reproduction of the affected lanes on hosted CI. A bounded exception
requires explicit Director authorization recorded in the ledger below with its
replacement evidence enumerated. A local PASS claim alone is never a satisfied
execution gate.

Hosted Actions executes on this account and is the CI surface. The workflow
routes to `ubuntu-latest`, `macos-14`, and `windows-latest`, with `macos-14`
reserved for the Metal storefronts (`gpui`, `ml-candle-metal`). A lane whose
`platforms` exclude this host is `skip`, named in the receipt, and is never
counted as `pass`. Portable claims rest on the three-OS matrix actually
executing.

**Publishing is manual.** `crates.io` publish requires a human-held token. A
green dry-run is not evidence that publishing works. `docs/releasing.md` is the
release process; a release cut is a Director action.

---

## 4. Change control for governance

`GOVERNANCE.md`, `CODEBOOK.md`, `WORKFLOW.md`, `AGENTS.md`, `INVARIANTS.md`,
and `REQUIREMENTS.md` are the governance surface. Changing them is a
governance change and follows these rules:

1. A governance change ships in its own commit, separate from the semantic change
   it describes, unless the two are inseparable.
2. It records *why* in the ledger below, with the date and the decision it
   implements or supersedes.
3. It never weakens a gate to clear a refusal. Widening `allowed_consumers`,
   editing a pinned version, adding an `APPROVED.toml` row, or loosening a lint
   level to pass a check is a suppressed gate, and it is a defect.
4. It never turns a `forbid` into a `deny` except through the documented
   exceptions in `CODEBOOK.md` §2.1.
5. Retiring a document moves it to `docs/archive/` and registers the
   supersession here. There is never a second live copy.
6. This repository's dependency doctrine (`AGENTS.md`, `docs/dependency-doctrine.md`,
   `contract/APPROVED.toml`) is a *narrower* rule layered on top of `CODEBOOK.md`
   §10. Where they conflict on a dependency question, the narrower rule wins.

---

## 5. Completion and decision ledger

Newest first. Each entry names what changed, the receipts, and what it does
**not** claim.

### 2026-09-23 — Requirements spine, bot durability fixes, readiness

- **Requirements spine.** `REQUIREMENTS.md` (R1–R13, immutable, superseded
  never edited), `scripts/check-requirements.py`, and
  `scripts/requirements.lock` land as governance as code, wired into
  `scripts/gate-lanes.toml` (`requirements` lane) and the `contract-drift`
  CI job. `INVARIANTS.md` is tracked as the short enforced rule list.
- Merged PRs
  [#112](https://github.com/srinji-kaggss/logicalworks-crates/pull/112)
  through [#140](https://github.com/srinji-kaggss/logicalworks-crates/pull/140)
  after the #88–#111 ledger line:
  #112 journal-before-acknowledge, #113 recovered-unknown barrier, #114
  dispatch-digest-to-admitted-input, #115 real-dispatch-path handoff, #116
  locator-ladder eligibility, #117 descendant process cleanup, #121 effect
  kernel architecture, #124 red-team leftovers, #125 four-file suite and
  lane parity, #128 non-executable process descriptions, #132
  enable-time deadline, #138 edge-identity watch, #139 read-failure-is-error,
  #140 nine-axis readiness and RPA case.
- The bot invariants INV-BOT-1..11 already name these defects; INV-GOV-1
  (gate defined once) and INV-GOV-2 (requirements superseded, never edited)
  are added in this change.

**Does not claim:** hosted CI reproduction of the new lane; crash-kill
evidence for R10; Frontier/Performance matched comparison.

### 2026-09-21 — Governance suite and local CI (this change)

This entry records the adoption of the estate governance suite (`CODEBOOK.md`,
`WORKFLOW.md`, this charter), the local CI gate (`scripts/ci-local.sh`), and the
retroactive backfill of the ledger below. The suite is copied from
`logical-DB/GOVERNANCE.md` / `CODEBOOK.md` / `WORKFLOW.md` on 2026-09-21 and
maintained here as this repository's copy. The dependency doctrine in `AGENTS.md`
and `docs/dependency-doctrine.md` is unchanged and remains the narrower rule.

**Does not claim:** any behaviour change to the four crates; a release; hosted
CI evidence; that the cross-OS matrix lanes (Linux, Windows) have been
reproduced on this machine.

### 2026-09-22 — Bot durability, surface narrowing, locator ladder

Merged PRs [#88](https://github.com/srinji-kaggss/logicalworks-crates/pull/88)
through [#111](https://github.com/srinji-kaggss/logicalworks-crates/pull/111):

| PR | Change |
|---|---|
| [#111](https://github.com/srinji-kaggss/logicalworks-crates/pull/111) | `fix(lgwks_bot)`: keep a known effect outcome when its record fails to land |
| [#110](https://github.com/srinji-kaggss/logicalworks-crates/pull/110) | `fix(lgwks_bot)`: commit observation fingerprints only with their values |
| [#105](https://github.com/srinji-kaggss/logicalworks-crates/pull/105) | `docs(frontier)`: name the effect postcondition read as the piece no one else has |
| [#103](https://github.com/srinji-kaggss/logicalworks-crates/pull/103) | `feat(bot)`: the locator ladder, so a search order is typed rather than implied |
| [#98](https://github.com/srinji-kaggss/logicalworks-crates/pull/98) | `refactor!`: narrow the public surface to one path per item |
| [#97](https://github.com/srinji-kaggss/logicalworks-crates/pull/97) | `feat(bot)`: an ephemeral scope, and a std-first audit of the source |
| [#96](https://github.com/srinji-kaggss/logicalworks-crates/pull/96) | `chore(docs)`: pin every citation to the line it names, so a move fails |
| [#95](https://github.com/srinji-kaggss/logicalworks-crates/pull/95) | `ci`: run the matrix once per change, and make the doc gate runnable locally |
| [#94](https://github.com/srinji-kaggss/logicalworks-crates/pull/94) | `fix(bot)!`: mark the three builders that could be dropped, and state authority |
| [#93](https://github.com/srinji-kaggss/logicalworks-crates/pull/93) | `feat(bot)!`: make effect dispatch durable and authorized, settle by `EffectKey` |
| [#92](https://github.com/srinji-kaggss/logicalworks-crates/pull/92) | `feat(api)!`: align the public surface with the Rust API guidelines; complete `Debug` |
| [#90](https://github.com/srinji-kaggss/logicalworks-crates/pull/90) | `feat(bot)`: a registry for what a spec's identifiers resolve to |
| [#89](https://github.com/srinji-kaggss/logicalworks-crates/pull/89) | `feat(session)`: read a flow in RON, through the one validation path |
| [#88](https://github.com/srinji-kaggss/logicalworks-crates/pull/88) | `docs(bot)`: task + script DX for declarative async orchestration |

Public-surface breaks (`refactor!`, `feat(api)!`, `feat(bot)!`, `fix(bot)!`) are
semver-major for a `0.x` crate and are recorded in `CHANGELOG.md` under the
affected crate. The one-path-per-item narrowing (#98) removes re-export
duplication so each item has exactly one public path.

**Does not claim:** that the narrowed surface is semver-stable; that the effect
dispatch durability has been exercised under a real crash; a performance
superiority claim from the `bench(bot)` baseline (#79).

### 2026-09-21 — Release cut and bot effect identity

- **[PR #91](https://github.com/srinji-kaggss/logicalworks-crates/pull/91)
  release cut**: `lgwks_std` 0.6.7, `lgwks_bot` 0.5.0, `lgwks_deps` 0.1.13, and
  the root `LICENSE`. Publish is manual and requires a human-held crates.io
  token; the cut is the tag and the changelog, not a successful publish.
- [#86](https://github.com/srinji-kaggss/logicalworks-crates/pull/86) `feat(effect)`:
  durable effect identity for settlements.
- [#85](https://github.com/srinji-kaggss/logicalworks-crates/pull/85) `chore`:
  bring `context7.json` and the changelog back in line with what shipped.
- [#84](https://github.com/srinji-kaggss/logicalworks-crates/pull/84) `perf(lgwks-bot)`:
  stop the tick rebuilding its own state (SPEC-12, allocations).
- [#83](https://github.com/srinji-kaggss/logicalworks-crates/pull/83) `feat(lgwks-bot)`:
  fingerprint the source, and stop polling what holds still.
- [#81](https://github.com/srinji-kaggss/logicalworks-crates/pull/81) `feat(lgwks-bot)`:
  type the observe builder against its source, witness the erasure, classify a
  dispatch failure (SPEC-12 PR-B).
- [#80](https://github.com/srinji-kaggss/logicalworks-crates/pull/80) `lgwks-bot`:
  bind settlement, abandonment and payload to the transition that owns them
  (F02/F03/F04).
- [#79](https://github.com/srinji-kaggss/logicalworks-crates/pull/79) `bench(bot)`:
  measure the bot against a baseline, and machine-check the design claims.
- [#78](https://github.com/srinji-kaggss/logicalworks-crates/pull/78) `feat(lgwks-bot)!`:
  make a supervisor the only way to start work or a process.

**Does not claim:** that the published crates.io artifacts match this tree
(publish is manual and a human step); a Frontier or Performance nine-axis green
from #79's baseline.

### 2026-09-21 — Authority, capability shortfall, citations

- [#77](https://github.com/srinji-kaggss/logicalworks-crates/pull/77)
  `feat(lgwks_bot)`: report the whole capability shortfall, and derive its repair.
- [#76](https://github.com/srinji-kaggss/logicalworks-crates/pull/76)
  `docs(lgwks-bot)`: record four owner decisions and correct three stale claims.
- [#75](https://github.com/srinji-kaggss/logicalworks-crates/pull/75)
  `docs`: make the citations checkable, consolidate the changelog, fix a
  vendor-fixture race.
- [#74](https://github.com/srinji-kaggss/logicalworks-crates/pull/74)
  `fix(lgwks-bot)`: report how supervised tasks end, honour the authority
  snapshot, record a verdict's provenance (#41, #42, #52).
- [#73](https://github.com/srinji-kaggss/logicalworks-crates/pull/73)
  `docs(licence)`: close `lgwks_bot` to outside contributions, defer the CLA
  instrument.
- [#72](https://github.com/srinji-kaggss/logicalworks-crates/pull/72)
  `fix(lgwks-bot)`: stop the tick parking a reactor thread, unstarve
  cancellation, flatten the cancel tree (#33, #43, #47).

`lgwks_bot` is closed to outside contributions (PR #73). `lgwks_bot` is MPL-2.0
and the other three crates carry their own licence map (PR #63, DOC-05). The
root `LICENSE` landed in the PR #91 release cut.

**Does not claim:** that the deferred CLA instrument exists.

### 2026-09-21 — Gate honesty and bounded reads

The 2026-09-21 defect campaign closed a class of gates that reported success for
states they never examined:

| PR | Defect class |
|---|---|
| [#71](https://github.com/srinji-kaggss/logicalworks-crates/pull/71) | a bounded HTTP read, statement-level scan evidence, and a check that cannot audit the wrong tree (#44, #46, #51) |
| [#70](https://github.com/srinji-kaggss/logicalworks-crates/pull/70) | a clean tick means nothing is held: frontier permits and the lost-work ledger (#23, #27, #30) |
| [#69](https://github.com/srinji-kaggss/logicalworks-crates/pull/69) | tier precedence, alias identity, reading an integer as a value (#25, #37, #38) |
| [#68](https://github.com/srinji-kaggss/logicalworks-crates/pull/68) | bound bytes not just steps, execute declared terminals, validate ask options (#29, #35, #40) |
| [#67](https://github.com/srinji-kaggss/logicalworks-crates/pull/67) | reads before init, and the scanner's two-sided evidence gap (#36, #48, #49) |
| [#66](https://github.com/srinji-kaggss/logicalworks-crates/pull/66) | `fix(deps)`: the gate reported success for states it never examined (#34, #45, #54) |
| [#65](https://github.com/srinji-kaggss/logicalworks-crates/pull/65) | three resolvers that reported more confidence than their evidence (#31, #39, #50) |
| [#64](https://github.com/srinji-kaggss/logicalworks-crates/pull/64) | `test(ast)`: execute the grammar matrix instead of assuming Rust is in it (#53) |

The `lgwks-deps` gate no longer reports success for a state it never examined
(#66). The scanner's evidence is statement-level and cannot audit the wrong tree
(#71). These are the precedents behind `CODEBOOK.md` §12 and the gate-honesty
rule in §4 of this charter.

### 2026-09-21 — Documentation and licence foundations

- [#63](https://github.com/srinji-kaggss/logicalworks-crates/pull/63)
  `chore(licence)`: MPL-2.0 for `lgwks_bot`, and the licence map (DOC-05).
- [#62](https://github.com/srinji-kaggss/logicalworks-crates/pull/62)
  `fix(docs)`: make the generated index honest about what it links.
- [#61](https://github.com/srinji-kaggss/logicalworks-crates/pull/61)
  `docs(guides)`: a consumer guide corpus for all four crates.

### Standing dependency doctrine (2026-09-12, unchanged)

Three dependency surfaces and one standalone parser:

- `lgwks_std`: the core surface.
- `lgwks_bot`: async, runners, and the actor roles.
- `lgwks_deps`: everything else, the **storefront**. The end user installs
  `lgwks_deps` and selects which dependency features to turn on.
- `lgwks_ast`: standalone. Grandfathered; do not grow it into a fourth surface.

If a capability exists in `std`, `lgwks_std`, `lgwks_bot`, or `lgwks_ast`, use
it. A **new** third-party dependency is an optional, feature-gated edge of
`lgwks_deps`, never built unless the end user selects it, registered in
`contract/APPROVED.toml` with `owner = "lgwks_deps"`. `lgwks-deps check .`
refuses every authored external edge with no owner, and refuses an approval with
no authored edge. `lgwks_std` cannot route through `lgwks_deps`, because
`lgwks_deps` depends on `lgwks_std` and the reverse would be a cycle.

Do **not** add `tokio`, `futures`, `async-trait`, `pollster`, `syn`,
`proc-macro2`, `regex`, `uuid`, `chrono`, `walkdir`, `glob`, `base64`, `hex`,
`percent-encoding`, `serde_json`, `ureq`, `reqwest`, or `ast-grep-*` directly.
Each maps to a workspace path; see `docs/dependency-doctrine.md`. Async and
runners are exposed by `lgwks_bot`; its tokio engine is selected through the
`lgwks_deps` storefront, which owns the workspace's one `tokio` edge. The gate
refuses a second `tokio` consumer.

---

## 6. Open correctness and acceptance work

| Priority | Work | Observed gap and completion evidence |
|---|---|---|
| P0 | Publish verification | `crates.io` publish is manual and needs a human-held token. A green dry-run is not evidence. Reconcile the published artifacts against the PR #91 tag |
| P0 | Cross-OS lane reproduction | The three-OS matrix (`ubuntu-latest`, `macos-14`, `windows-latest`) executes on hosted Actions and is the Portable evidence. A prior draft collapsed the routes onto one self-hosted macOS rung; that was a coverage regression and is not landing. `scripts/check-gate-parity.py` binds lane ids and commands to the workflow rather than policing runner labels |
| P1 | Effect-dispatch crash exercise | #93 made effect dispatch durable and authorized. A real crash-during-settlement journey is not in the suite |
| P1 | Narrowed-surface semver audit | #98 is `refactor!`. Confirm no downstream consumer inside the estate is broken, and record the break in `CHANGELOG.md` |
| P2 | Frontier / Performance evidence | #79 carries a baseline. No matched architecture-class comparison, no allocation or contention profiling |
| Deferred | CLA instrument | #73 deferred it. `lgwks_bot` is closed to outside contributions meanwhile |
| Standing | Dependency admission | Any new third-party edge goes through `skills/lgwks-dependency-admission/SKILL.md` and lands in `contract/APPROVED.toml` |

---

## 7. Decisions requiring the Director

| Decision | Conflicting authority or missing choice | Rule until decided |
|---|---|---|
| Publish of the PR #91 cut | Human-held crates.io token | The tag and changelog exist. Published artifacts are not claimed to match the tree |
| Merge without executed CI reproduction | A local PASS is not a CI run | Local receipts support source review only. Merge eligibility needs executed CI reproduction on the affected lanes unless the Director records a bounded exception naming its replacement evidence |
| Licence change or re-opening `lgwks_bot` | #73 closed it to outside contributions; the CLA instrument is deferred | `lgwks_bot` stays closed. The other three crates keep their licence map from #63 |

A required decision pauses dependent work. Audits, reproducible counterexamples,
and factual documentation may continue.
