# Documentation

Depth lives in [``](), not in this file. If you are here to use a
crate rather than to change one, start with the [consumer guides](guides/index.md);
the rest of this table is design history and contributor policy.

| Document | Covers |
|---|---|
| [`overview.md`](overview.md) | What the crates are for, what they are not, and how they relate to the crates a reader meets first. |
| [`choosing-a-crate.md`](choosing-a-crate.md) | The dependency graph stated exactly, and which crate and feature to reach for. |
| [`guides/index.md`](guides/index.md) | Choose a crate, and which symbols are stable versus main-only. |
| [`guides/lgwks-bot/index.md`](guides/lgwks-bot/index.md) | `lgwks_bot`: purpose, non-goals, and the supported versions. |
| [`guides/lgwks-bot/getting-started.md`](guides/lgwks-bot/getting-started.md) | One source, one condition, one effect, compiled and run. |
| [`guides/lgwks-bot/features.md`](guides/lgwks-bot/features.md) | Every `lgwks_bot` feature, and what `--no-default-features` does not remove. |
| [`guides/lgwks-bot/authority.md`](guides/lgwks-bot/authority.md) | Admission, the `Auth` snapshot, and the no-sandbox boundary. |
| [`guides/lgwks-bot/failures.md`](guides/lgwks-bot/failures.md) | Poll failure, partial action failure, and unknown effect. |
| [`guides/lgwks-bot/background-work.md`](guides/lgwks-bot/background-work.md) | Bounded fan-out, supervision, and the cancellation limits. |
| [`guides/lgwks-bot/migrating-to-1-0.md`](guides/lgwks-bot/migrating-to-1-0.md) | What 1.0.0 removed, what replaces it, and the traps the compiler cannot show. |
| [`guides/lgwks-bot/sessions.md`](guides/lgwks-bot/sessions.md) | Unreleased: validated guidance flows and the session runner. |
| [`guides/lgwks-bot/resolution.md`](guides/lgwks-bot/resolution.md) | Unreleased: lexical and semantic resolution, and degraded verdicts. |
| [`guides/lgwks-bot/domains.md`](guides/lgwks-bot/domains.md) | Unreleased: the domain registry, and what a spec's identifiers resolve to. |
| [`guides/lgwks-std/index.md`](guides/lgwks-std/index.md) | `lgwks_std`: install, feature selection, and logging. |
| [`guides/lgwks-ast/index.md`](guides/lgwks-ast/index.md) | `lgwks_ast`: install, grammar selection, bounds, and diagnostics. |
| [`guides/lgwks-ast/grammars.md`](guides/lgwks-ast/grammars.md) | Every grammar feature and its extensions, the derive-only build, and custom languages. |
| [`guides/lgwks-ast/bounds.md`](guides/lgwks-ast/bounds.md) | Each `lgwks_ast` ceiling: its unit, what it bounds, and what it does not promise. |
| [`guides/lgwks-deps/index.md`](guides/lgwks-deps/index.md) | `lgwks_deps`: install, the storefront features, and the CLI. |
| [`guides/lgwks-deps/embedding.md`](guides/lgwks-deps/embedding.md) | Embedding the gate as a library, and adopting the policy in your own workspace. |
| [`guides/lgwks-deps/register.md`](guides/lgwks-deps/register.md) | The register's strict TOML syntax, the `tier` control, and what `vendor check` binds. |
| [`dependency-doctrine.md`](dependency-doctrine.md) | How a dependency is admitted, the replacement matrix, and where no equivalent exists. Read this before adding a crate. |
| [`async-parity.md`](async-parity.md) | The `lgwks_bot::rt` surface against tokio and the alternatives: the capability matrix, the open gaps, and the deliberate divergences. |
| [`async-sdk-shape.md`](async-sdk-shape.md) | The async surface as an SDK: its shape, its invariants, and how to migrate. |
| [`declarative-orchestration.spec.md`](declarative-orchestration.spec.md) | The task-first front door: typed task, host and report, one registry and execution path, the responsibility split, and the full known-deficit repair. Proposed, not shipped. |
| [`orchestration-lifecycle.spec.md`](orchestration-lifecycle.spec.md) | Single-owner state, atomic fingerprint and value commits, attempt-bound evidence, readiness and process-tree ownership, and the honest durability boundary. |
| [`script-language.spec.md`](script-language.spec.md) | The `script!` language: one lexicon, one parser, one call per line, guarantees carried by words, and where the parser lives. Proposed, not shipped. |
| [`adr/0001-one-rust-reader.md`](adr/0001-one-rust-reader.md) | ADR 0001: Rust is read by one parser, `lgwks_ast`. The decision is backed by fidelity, latency, peak-RSS and hostile-nesting measurements on this repository's own sources; the `scan` gate reads through it (#408). |
| [`ai-orchestration.spec.md`](ai-orchestration.spec.md) | AI-authored scripts and AI workloads: constrained proposals, bounded artifact context, subject pinning, no-progress intervention and same-model evaluation. |
| [`pr-review-orchestration.spec.md`](pr-review-orchestration.spec.md) | The reference journey: a pinned snapshot through script, checked publication and lost-response reconciliation, and its explicit non-CAS freshness limit. |
| [`orchestration-acceptance.spec.md`](orchestration-acceptance.spec.md) | The 36 proposed public-interface falsifiers, and the developer-experience and quality evaluation that would accept the facade. Every case unrun. |
| [`script-lexicon.md`](script-lexicon.md) | Every `script!` word on one page, generated from the lexicon the parser dispatches on: its forms, refusals, guarantees and an example. Enough context to write a script from alone. |
| [`orchestration-evidence.md`](orchestration-evidence.md) | The pinned source findings, the foundational Rust assessment, and the reference entries the orchestration specs are built on. |
| [`bot-on-ecs.md`](bot-on-ecs.md) | Bot semantics on an ECS substrate, and what that mapping costs. |
| [`effect-kernel.md`](effect-kernel.md) | The durable effect dispatch and recovery kernel: the journal ladder, the recovery fold, settlement unification, durability admission, and what one authority means. |
| [`bevy-admission.md`](bevy-admission.md) | The Bevy ECS decision and the alternatives measured against it. |
| [`candle-admission.md`](candle-admission.md) | ML inference admission: authority, transitive surface, verification. |
| [`appcui-admission.md`](appcui-admission.md) | Native terminal UI admission: authority and verification. |
| [`distributed-boundaries.md`](distributed-boundaries.md) | What these crates do and refuse on a distributed network path. Read it before placing them on one. |
| [`general-bot-fold.md`](general-bot-fold.md) | How a full browser recorder-and-replayer architecture folds into this bot: what already is it, what becomes a seam, and what is refused. |
| [`security-posture.md`](security-posture.md) | The security case: the execution-locus claim and its ceiling, what the browser integrity stack does and does not prove, and the evidence a reviewer can check without trusting the vendor. |
| [`guidance-runner-spec.md`](guidance-runner-spec.md) | The session, flow and invariant-register workstreams: what each delivers, the seam every borrowed part sits behind, and the corrections two research sweeps made to the original design. |
| [`estate-asset-inventory.md`](estate-asset-inventory.md) | The adjacent repositories searched before any of it was written, what was taken, and what was deliberately left behind. |
| [`frontier.md`](frontier.md) | The state of the art across the nine areas this bot competes in, with measured anchors and the design decision each forces. |
| [`framework-comparison.md`](framework-comparison.md) | Why the field looks the same, the four axes it actually differs on, and the axis nobody occupies. |
| [`production-readiness.md`](production-readiness.md) | Nine-axis production readiness of `lgwks_bot`, the case that it replaces non-AI automation and RPA, and the rows that currently fail their own gate. |
| [`std-ast-deps-closure-matrix.md`](std-ast-deps-closure-matrix.md) | All twenty-one `lgwks_std` modules, `lgwks_ast` and `lgwks_deps`: each with its feature, its evidence state, the test that exists on the named revision, and its owning issue. |
| [`invariants-long-form.md`](invariants-long-form.md) | The reasoning, measurements and stated limits behind each one-line rule in `INVARIANTS.md`. |
| [`releasing.md`](releasing.md) | The release process. |
| [`CHANGELOG.md`](../CHANGELOG.md) | Per-release changes across all five crates. |
| [`SECURITY.md`](../SECURITY.md) | Attack surface, reporting process, and advisories assessed. |

Each crate also carries its own README: [`lgwks_std`](../crates/lgwks-std/README.md),
[`lgwks_bot`](../crates/lgwks-bot/README.md),
[`lgwks_ast`](../crates/lgwks-ast/README.md),
[`lgwks_deps`](../crates/lgwks-deps/README.md).
