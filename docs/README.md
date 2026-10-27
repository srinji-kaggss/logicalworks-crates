# Documentation

The index of every document in this repository. The `llms-txt` binary of `lgwks_deps`
builds `llms.txt` from the table below and fails when a document under
`docs/` is missing from it.

## Documentation

Depth lives in [`docs/`](), not in this file. If you are here to use a
crate rather than to change one, start with the [consumer guides](guides/index.md);
the rest of this table is design history and contributor policy.

| Document | Covers |
|---|---|
| [`docs/use-cases.md`](use-cases.md) | What each crate is for, what none of them is, how they differ from the crates a search finds first, agent security and in-process background work. |
| [`docs/choosing-a-crate.md`](choosing-a-crate.md) | Which crate and feature for a need, the exact dependency graph, and the default-feature caveat on the zero-dependency core. |
| [`docs/should-i-use.md`](should-i-use.md) | The decision question a reader arrives with: who publishes this, when to use each crate, when not to, and what to use instead. |
| [`docs/guides/index.md`](guides/index.md) | Choose a crate, the versions in this tree, and the supported interface. |
| [`docs/guides/lgwks-bot/index.md`](guides/lgwks-bot/index.md) | `lgwks_bot`: purpose, non-goals, and the supported versions. |
| [`docs/guides/lgwks-bot/getting-started.md`](guides/lgwks-bot/getting-started.md) | One source, one condition, one effect, compiled and run. |
| [`docs/guides/lgwks-bot/features.md`](guides/lgwks-bot/features.md) | Every `lgwks_bot` feature, and what `--no-default-features` does not remove. |
| [`docs/guides/lgwks-bot/authority.md`](guides/lgwks-bot/authority.md) | Admission, the `Auth` snapshot, and the no-sandbox boundary. |
| [`docs/guides/lgwks-bot/failures.md`](guides/lgwks-bot/failures.md) | Poll failure, partial action failure, and unknown effect. |
| [`docs/guides/lgwks-bot/background-work.md`](guides/lgwks-bot/background-work.md) | Bounded fan-out, supervision, and the cancellation limits. |
| [`docs/guides/lgwks-bot/migrating-to-1-0.md`](guides/lgwks-bot/migrating-to-1-0.md) | What 1.0.0 removed, what replaces it, and the traps the compiler cannot show. |
| [`docs/guides/lgwks-bot/sessions.md`](guides/lgwks-bot/sessions.md) | Support code (`#[doc(hidden)]`): validated guidance flows and the session runner. |
| [`docs/guides/lgwks-bot/resolution.md`](guides/lgwks-bot/resolution.md) | Support code (`#[doc(hidden)]`): lexical and semantic resolution, and degraded verdicts. |
| [`docs/guides/lgwks-bot/domains.md`](guides/lgwks-bot/domains.md) | Support code (`#[doc(hidden)]`): the domain registry, and what a spec's identifiers resolve to. |
| [`docs/guides/lgwks-std/index.md`](guides/lgwks-std/index.md) | `lgwks_std`: install, feature selection, and logging. |
| [`docs/guides/lgwks-ast/index.md`](guides/lgwks-ast/index.md) | `lgwks_ast`: install, identify, parse and inspect, and where to go next. |
| [`docs/guides/lgwks-ast/bounds-and-refusals.md`](guides/lgwks-ast/bounds-and-refusals.md) | The three constants a checked parse applies and the four refusals each one answers. |
| [`docs/guides/lgwks-ast/custom-and-diagnostics.md`](guides/lgwks-ast/custom-and-diagnostics.md) | A caller-registered grammar and the shared typed-diagnostic derive. |
| [`docs/guides/lgwks-ast/grammars.md`](guides/lgwks-ast/grammars.md) | Grammar selection by cargo feature, and the extension table each build compiles. |
| [`docs/guides/lgwks-ast/script-and-custom.md`](guides/lgwks-ast/script-and-custom.md) | The `script!` reader and the custom-language entry points of `lgwks_ast`. |
| [`docs/guides/lgwks-ast/bounds.md`](guides/lgwks-ast/bounds.md) | Every bound the checked parse and the inspection walk apply, with the measurements behind them. |
| [`docs/guides/lgwks-deps/index.md`](guides/lgwks-deps/index.md) | `lgwks_deps`: library use versus CLI and policy adoption. |
| [`docs/guides/lgwks-deps/storefront.md`](guides/lgwks-deps/storefront.md) | Every `lgwks_deps` storefront feature, what it re-exports, and what each pulls in. |
| [`docs/guides/lgwks-deps/embedding.md`](guides/lgwks-deps/embedding.md) | Embedding `check_dependencies` in your own tool, and when adopting the policy is the right call. |
| [`docs/dependency-doctrine.md`](dependency-doctrine.md) | How a dependency is admitted, the replacement matrix, and where no equivalent exists. Read this before adding a crate. |
| [`docs/deferral.md`](deferral.md) | The deferred crate catalog: what waits for day 0, each row's owner and measured trigger. |
| [`docs/patches.md`](patches.md) | Upstream pins that never become forks, and the TLS provider path with its sealed exceptions. |
| [`docs/async-parity.md`](async-parity.md) | The `lgwks_bot::rt` surface against tokio and the alternatives: the capability matrix, the open gaps, and the deliberate divergences. |
| [`docs/async-sdk-shape.md`](async-sdk-shape.md) | The async surface as an SDK: its shape, its invariants, and how to migrate. |
| [`docs/declarative-orchestration.spec.md`](declarative-orchestration.spec.md) | The task-first front door: typed task, host and report, and straight-line typed results. Implemented in part as `lgwks_bot::task`; each requirement is owned by the falsifier rows that require it. |
| [`docs/declarative-orchestration-composition.spec.md`](declarative-orchestration-composition.spec.md) | Part two of the task-first specification: composition, admission and repair, one registry and execution path, ownership and visibility. |
| [`docs/orchestration-lifecycle.spec.md`](orchestration-lifecycle.spec.md) | Single-owner state, atomic fingerprint and value commits, attempt-bound evidence, readiness and process-tree ownership, and the honest durability boundary. |
| [`docs/script-language.spec.md`](script-language.spec.md) | The `script!` language: one lexicon, one parser, one call per line, guarantees carried by words, and where the parser lives. Proposed, not shipped. |
| [`docs/catalog.md`](catalog.md) | Every capability the estate owns, its owner path, its crate, its feature and whether it is on by default. Generated from `contract/CATALOG.toml`; the std-first gate's refusals name rows in it. |
| [`docs/adr/README.md`](adr/README.md) | The architecture decision record contract: every record against ISO/IEC/IEEE 42010, the fields it must hold, and the index of records. |
| [`docs/adr/0001-one-rust-reader.md`](adr/0001-one-rust-reader.md) | ADR 0001: Rust is read by one parser, `lgwks_ast`. The decision is backed by fidelity, latency, peak-RSS and hostile-nesting measurements on this repository's own sources. |
| [`docs/adr/0002-scan-routed-through-lgwks-ast.md`](adr/0002-scan-routed-through-lgwks-ast.md) | ADR 0002: the `scan` gate reads Rust through `lgwks_ast`, so `lgwks_deps` no longer carries `syn`, and `lgwks-deps scan` points at `lgwks-ast scan` (#408). |
| [`docs/adr/0003-every-capability-on-by-default.md`](adr/0003-every-capability-on-by-default.md) | ADR 0003: every capability is on by default, with the measured cost of the default and the three rows an opt-out is justified for (#732). |
| [`docs/adr/0004-documentation-governance.md`](adr/0004-documentation-governance.md) | ADR 0004: ADRs are held to ISO/IEC/IEEE 42010 and PRDs to ISO/IEC/IEEE 29148, read by `check-docs` in the `doc-governance` lane. |
| [`docs/adr/0005-bot-core-is-an-event-driven-ecs.md`](adr/0005-bot-core-is-an-event-driven-ecs.md) | ADR 0005: the bot core is an event-driven ECS with tokio and the executors as drivers below it; the five-step refactor (seal, wake, run events, drishti in, measure) and the rule that every admitted engine is replaced by ours on a matched benchmark. |
| [`docs/adr/measurements/0001-rust-readers.md`](adr/measurements/0001-rust-readers.md) | The corpus, commands and result tables behind ADR 0001. |
| [`docs/adr/measurements/0002-scan-route-measurement.md`](adr/measurements/0002-scan-route-measurement.md) | The wall-time, peak-RSS and hostile-nesting tables behind ADR 0002. |
| [`docs/prd/README.md`](prd/README.md) | The product requirement document contract: every requirement against ISO/IEC/IEEE 29148, the attributes it must carry, and the index of documents. |
| [`docs/prd/0001-discoverable-opinionated-estate.md`](prd/0001-discoverable-opinionated-estate.md) | PRD 0001: opinionated defaults, one catalog, the build-script surface, complete tracing, gate-catalog agreement and the published page (#732). |
| [`docs/ai-orchestration.spec.md`](ai-orchestration.spec.md) | AI-authored scripts and AI workloads: constrained proposals, bounded artifact context, subject pinning, no-progress intervention and same-model evaluation. |
| [`docs/pr-review-orchestration.spec.md`](pr-review-orchestration.spec.md) | The reference journey: a pinned snapshot through script, checked publication and lost-response reconciliation, and its explicit non-CAS freshness limit. |
| [`docs/orchestration-acceptance.spec.md`](orchestration-acceptance.spec.md) | The 36 public-interface falsifiers, the requirement each one owns, and the per-revision table of the tests that drive them. No row is accepted yet. |
| [`docs/orchestration-acceptance-evaluation.md`](orchestration-acceptance-evaluation.md) | How falsifier evidence is graded, what candidate tests do not prove, and the developer-experience and multi-axis evaluation that would accept the facade. |
| [`docs/script-lexicon.md`](script-lexicon.md) | Every `script!` word on one page, generated from the lexicon the parser dispatches on: its forms, refusals, guarantees and an example. Enough context to write a script from alone. |
| [`docs/script-word-admission.md`](script-word-admission.md) | How a word joins the `script!` language: three cited estate sites, its row, a refusal case per refusal, a test per guarantee, a seeded simulation, and the passthrough census that finds candidates. |
| [`docs/orchestration-evidence.md`](orchestration-evidence.md) | The pinned source findings, the foundational Rust assessment, and the reference entries the orchestration specs are built on. |
| [`docs/bot-on-ecs.md`](bot-on-ecs.md) | Bot semantics on an ECS substrate, and what that mapping costs. |
| [`docs/effect-kernel.md`](effect-kernel.md) | The durable effect dispatch and recovery kernel: the journal ladder, the recovery fold, settlement unification, durability admission, and what one authority means. |
| [`docs/bevy-admission.md`](bevy-admission.md) | The Bevy ECS decision and the alternatives measured against it. |
| [`docs/bevy-0.20-spike.md`](bevy-0.20-spike.md) | The admission's spike re-run on `bevy_ecs` 0.20: the deferred-effect control, `Changed<T>`, the slot-count caveat, and the cost. |
| [`docs/candle-admission.md`](candle-admission.md) | ML inference admission: authority, transitive surface, verification. |
| [`docs/appcui-admission.md`](appcui-admission.md) | Native terminal UI admission: authority and verification. |
| [`docs/distributed-boundaries.md`](distributed-boundaries.md) | What these crates do and refuse on a distributed network path. Read it before placing them on one. |
| [`docs/general-bot-fold.md`](general-bot-fold.md) | How a full browser recorder-and-replayer architecture folds into this bot: what already is it, what becomes a seam, and what is refused. |
| [`docs/security-posture.md`](security-posture.md) | The security case: the execution-locus claim and its ceiling, what the browser integrity stack does and does not prove, and the evidence a reviewer can check without trusting the vendor. |
| [`docs/guidance-runner-spec.md`](guidance-runner-spec.md) | The session, flow and invariant-register workstreams: what each delivers, the seam every borrowed part sits behind, and the corrections two research sweeps made to the original design. |
| [`docs/estate-asset-inventory.md`](estate-asset-inventory.md) | The adjacent repositories searched before any of it was written, what was taken, and what was deliberately left behind. |
| [`docs/frontier.md`](frontier.md) | The state of the art across the nine areas this bot competes in, with measured anchors and the design decision each forces. |
| [`docs/framework-comparison.md`](framework-comparison.md) | Why the field looks the same, the four axes it actually differs on, and the axis nobody occupies. |
| [`docs/production-readiness.md`](production-readiness.md) | Nine-axis production readiness of `lgwks_bot`, the case that it replaces non-AI automation and RPA, and the rows that currently fail their own gate. |
| [`docs/std-ast-deps-closure-matrix.md`](std-ast-deps-closure-matrix.md) | All twenty-one `lgwks_std` modules, `lgwks_ast` and `lgwks_deps`: each with its feature, its evidence state, the test that exists on the named revision, and its owning issue. |
| [`docs/invariants-long-form.md`](invariants-long-form.md) | The reasoning, measurements and stated limits behind each one-line rule in `INVARIANTS.md`. |
| [`docs/releasing.md`](releasing.md) | The release process. |
| [`docs/governance/law.md`](governance/law.md) | Governance §2: the production engineering law. |
| [`docs/workflow/local-ci.md`](workflow/local-ci.md) | Workflow §1, §10 and §11: local CI, the runner map, and the mandatory WWFD cycle. |
| [`docs/workflow/delivery-loop.md`](workflow/delivery-loop.md) | Workflow §2 and §3: the delivery loop, its seven rules, and RAG for implementation expertise. |
| [`docs/workflow/one-path.md`](workflow/one-path.md) | Workflow §4 and §5: one path, one opinion, one architecture; compounding, buildable, surgical change. |
| [`docs/workflow/evidence.md`](workflow/evidence.md) | Workflow §6 and §7: evidence proportionality, the nine axes, and how work ends with a PR. |
| [`docs/workflow/agents.md`](workflow/agents.md) | Workflow §8 and §9: agent economics, data safety, and the five guard rules. |
| [`docs/workflow/ci-layout.md`](workflow/ci-layout.md) | Workflow §12: the CI budget, what sets the wall clock, and the nine axes the CI is held to. |
| [`docs/workflow/ci-runs.md`](workflow/ci-runs.md) | Workflow §12: the 25 measured CI runs, their layouts, wall clocks and verdicts. |
| [`docs/governance/gates.md`](governance/gates.md) | Governance §3 and §4: the gates, and change control for governance. |
| [`docs/governance/open-work.md`](governance/open-work.md) | Governance §6 and §7: open correctness and acceptance work, and the decisions requiring the Director. |
| [`docs/governance/ledger-2026-10-26.md`](governance/ledger-2026-10-26.md) | Governance §5 ledger, 2026-10-26: the newest part. |
| [`docs/governance/ledger-2026-10-21.md`](governance/ledger-2026-10-21.md) | Governance §5 ledger, 2026-10-21. |
| [`docs/governance/ledger-2026-10-09.md`](governance/ledger-2026-10-09.md) | Governance §5 ledger, 2026-10-09 to 2026-10-20. |
| [`docs/governance/ledger-2026-10-06.md`](governance/ledger-2026-10-06.md) | Governance §5 ledger, 2026-10-06 to 2026-10-08. |
| [`docs/governance/ledger-2026-09-28-releases.md`](governance/ledger-2026-09-28-releases.md) | Governance §5 ledger: the release cuts of 2026-09-28 to 2026-10-06. |
| [`docs/governance/ledger-2026-09-23.md`](governance/ledger-2026-09-23.md) | Governance §5 ledger, 2026-09-23 and 2026-09-27. |
| [`docs/governance/ledger-2026-09-21.md`](governance/ledger-2026-09-21.md) | Governance §5 ledger, 2026-09-21 and 2026-09-22. |
| [`docs/governance/ledger-2026-09-12.md`](governance/ledger-2026-09-12.md) | Governance §5 ledger: the 2026-09-21 campaigns and the standing doctrine of 2026-09-12. |
| [`CHANGELOG.md`](../CHANGELOG.md) | Per-release changes across all five crates. |
| [`SECURITY.md`](../SECURITY.md) | Attack surface, reporting process, and advisories assessed. |

Each crate also carries its own README: [`lgwks_std`](../crates/lgwks-std/README.md),
[`lgwks_bot`](../crates/lgwks-bot/README.md),
[`lgwks_ast`](../crates/lgwks-ast/README.md),
[`lgwks_deps`](../crates/lgwks-deps/README.md).
