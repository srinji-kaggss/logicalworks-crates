# Documentation

The index of every document in this repository. `scripts/gen-llms-txt.py`
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
| [`docs/guides/index.md`](guides/index.md) | Choose a crate, and which symbols are stable versus main-only. |
| [`docs/guides/lgwks-bot/index.md`](guides/lgwks-bot/index.md) | `lgwks_bot`: purpose, non-goals, and the supported versions. |
| [`docs/guides/lgwks-bot/getting-started.md`](guides/lgwks-bot/getting-started.md) | One source, one condition, one effect, compiled and run. |
| [`docs/guides/lgwks-bot/features.md`](guides/lgwks-bot/features.md) | Every `lgwks_bot` feature, and what `--no-default-features` does not remove. |
| [`docs/guides/lgwks-bot/authority.md`](guides/lgwks-bot/authority.md) | Admission, the `Auth` snapshot, and the no-sandbox boundary. |
| [`docs/guides/lgwks-bot/failures.md`](guides/lgwks-bot/failures.md) | Poll failure, partial action failure, and unknown effect. |
| [`docs/guides/lgwks-bot/background-work.md`](guides/lgwks-bot/background-work.md) | Bounded fan-out, supervision, and the cancellation limits. |
| [`docs/guides/lgwks-bot/migrating-to-1-0.md`](guides/lgwks-bot/migrating-to-1-0.md) | What 1.0.0 removed, what replaces it, and the traps the compiler cannot show. |
| [`docs/guides/lgwks-bot/sessions.md`](guides/lgwks-bot/sessions.md) | Unreleased: validated guidance flows and the session runner. |
| [`docs/guides/lgwks-bot/resolution.md`](guides/lgwks-bot/resolution.md) | Unreleased: lexical and semantic resolution, and degraded verdicts. |
| [`docs/guides/lgwks-bot/domains.md`](guides/lgwks-bot/domains.md) | Unreleased: the domain registry, and what a spec's identifiers resolve to. |
| [`docs/guides/lgwks-std/index.md`](guides/lgwks-std/index.md) | `lgwks_std`: install, feature selection, and logging. |
| [`docs/guides/lgwks-ast/index.md`](guides/lgwks-ast/index.md) | `lgwks_ast`: install, grammar selection, bounds, and diagnostics. |
| [`docs/guides/lgwks-deps/index.md`](guides/lgwks-deps/index.md) | `lgwks_deps`: library use versus CLI and policy adoption. |
| [`docs/dependency-doctrine.md`](dependency-doctrine.md) | How a dependency is admitted, the replacement matrix, and where no equivalent exists. Read this before adding a crate. |
| [`docs/async-parity.md`](async-parity.md) | The `lgwks_bot::rt` surface against tokio and the alternatives: the capability matrix, the open gaps, and the deliberate divergences. |
| [`docs/async-sdk-shape.md`](async-sdk-shape.md) | The async surface as an SDK: its shape, its invariants, and how to migrate. |
| [`docs/declarative-orchestration.spec.md`](declarative-orchestration.spec.md) | The task-first front door: typed task, host and report, one registry and execution path, the responsibility split, and the full known-deficit repair. Proposed, not shipped. |
| [`docs/orchestration-lifecycle.spec.md`](orchestration-lifecycle.spec.md) | Single-owner state, atomic fingerprint and value commits, attempt-bound evidence, readiness and process-tree ownership, and the honest durability boundary. |
| [`docs/script-language.spec.md`](script-language.spec.md) | The `script!` language: one lexicon, one parser, one call per line, guarantees carried by words, and where the parser lives. Proposed, not shipped. |
| [`docs/adr/0001-one-rust-reader.md`](adr/0001-one-rust-reader.md) | ADR 0001: Rust is read by one parser, `lgwks_ast`. The decision is backed by fidelity, latency, peak-RSS and hostile-nesting measurements on this repository's own sources; routing the `scan` gate through it is #408. |
| [`docs/ai-orchestration.spec.md`](ai-orchestration.spec.md) | AI-authored scripts and AI workloads: constrained proposals, bounded artifact context, subject pinning, no-progress intervention and same-model evaluation. |
| [`docs/pr-review-orchestration.spec.md`](pr-review-orchestration.spec.md) | The reference journey: a pinned snapshot through script, checked publication and lost-response reconciliation, and its explicit non-CAS freshness limit. |
| [`docs/orchestration-acceptance.spec.md`](orchestration-acceptance.spec.md) | The 36 proposed public-interface falsifiers, and the developer-experience and quality evaluation that would accept the facade. Every case unrun. |
| [`docs/script-lexicon.md`](script-lexicon.md) | Every `script!` word on one page, generated from the lexicon the parser dispatches on: its forms, refusals, guarantees and an example. Enough context to write a script from alone. |
| [`docs/script-word-admission.md`](script-word-admission.md) | How a word is admitted to `script!` (SL-7): three cited sites, the six things a row needs, and the passthrough census that signals a missing word. |
| [`docs/orchestration-evidence.md`](orchestration-evidence.md) | The pinned source findings, the foundational Rust assessment, and the reference entries the orchestration specs are built on. |
| [`docs/bot-on-ecs.md`](bot-on-ecs.md) | Bot semantics on an ECS substrate, and what that mapping costs. |
| [`docs/effect-kernel.md`](effect-kernel.md) | The durable effect dispatch and recovery kernel: the journal ladder, the recovery fold, settlement unification, durability admission, and what one authority means. |
| [`docs/bevy-admission.md`](bevy-admission.md) | The Bevy ECS decision and the alternatives measured against it. |
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
| [`CHANGELOG.md`](../CHANGELOG.md) | Per-release changes across all five crates. |
| [`SECURITY.md`](../SECURITY.md) | Attack surface, reporting process, and advisories assessed. |

Each crate also carries its own README: [`lgwks_std`](../crates/lgwks-std/README.md),
[`lgwks_bot`](../crates/lgwks-bot/README.md),
[`lgwks_ast`](../crates/lgwks-ast/README.md),
[`lgwks_deps`](../crates/lgwks-deps/README.md).
