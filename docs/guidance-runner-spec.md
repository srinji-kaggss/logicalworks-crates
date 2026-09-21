# Guidance runner specification

Status: draft for implementation. Three workstreams, file-disjoint, intended to
land as one reviewed stack.

## Purpose

`lgwks_bot` executes four verbs (Observe, Evaluate, Execute, Query) on a
change-detecting ECS schedule with proof-carrying authority. It is a substrate.
It has no session, no dialogue state machine, no variable scope, and no
declarative flow document, so it cannot carry the class of workload a digital
adoption platform carries: a deterministic, authored, human-facing conversation
with recorded outcomes.

This specification adds that layer. It does not change the four verbs, the
authority model, or the schedule.

## What already exists in the workspace, and what is deferred

The layer described here is not novel in this organization. A canonical flow
representation already exists elsewhere and is the reference design.

| Asset | Location | Status |
|---|---|---|
| `FlowSpec`, `FlowNodeKind`, `FlowEdge`, `TerminalOutcome`, `ChoiceArm`, `Predicate`, `ValueExpr`, `FlowBounds`, `JustificationDecl` | `sentinel/braid/crates/braid-flow-ir` | **Source may be copied.** Braid is deprecated (confirmed 2026-09-20) and the crate is MIT, owned by this organization. Copy where the code is self-contained; port where it is not |
| `ValueExpr` / `Predicate`, `src/predicate.rs` | same | Copy the design. It is a closed, side-effect-free expression language (const, Eq, Ne, Lt, Le, Gt, Ge, And, Or, Not) over a scalar. It needs `braid_ir::Value`, which this workspace does not have, so substitute this workspace's own scalar type. This is the branch-condition language Workstream B needs and it is 133 lines |
| `src/preflight.rs` | same | **Do not copy.** It is wire-byte hardening against untrusted canonical input (2 MiB-plus envelopes, nesting depth, container counts). It has no role here: this workspace parses its own documents through `lgwks_std::json`, not through an adversarial canonical decoder |
| `src/decode.rs`, `encode.rs`, `wire_depth.rs`, `literal.rs` | same | **Do not copy.** Canonical content-addressed wire encoding. It exists to give Braid flows a stable `Cid`, and it drags `braid-ir`'s `Cid`, `TypeTag` and `Value` with it. Nothing in this stack needs content-addressed flows |
| Declarative plan compilation with interpolation and lane resolution | `keel/src/construct/` | Reference design, same constraint |
| Sealed evidence snapshots (`toolchain`, `git`, `host`, `env_fingerprint`, `runner`) | `keel/src/evidence/audit_log/` | Reference design, same constraint |
| `Session`, `SessionId`, `SessionInvariant` | `secure-authority/crates/logicalworks-auth/src/session.rs` | Reference design for the session identity shape |

The consequence for this task: **port the shape, not the dependency.** Every
ported concept sits behind a trait so that when `braid-flow-ir` is published, or
when these modules are lifted into this workspace, each is replaced without
touching a call site. That is the design requirement the rest of this document
serves.

This table is not the whole picture. A full sweep of adjacent repositories is in
[`estate-asset-inventory.md`](estate-asset-inventory.md), and it found two assets
that are directly relevant: a journal-authoritative session runtime already
exists under a compatible licence, and so does the similarity scoring primitive.

**They are references, not replacements.** An earlier draft of this document
proposed collapsing Workstream B onto the existing runtime. That was rejected by
the maintainer, and the reason governs the whole design:

> The bot is the general instrument. It is not an implementation of one
> workload, and it does not get narrower because a sibling repository happens to
> solve a similar problem for a different purpose. The standard is the best bot
> that can be built, not the smallest one that satisfies today's consumer.

So the estate assets inform the *shape* — a bounded journal, an authoritative
fold, a revocation clock — while the bot keeps its own seams and its own
abstractions. Where a sibling asset is genuinely the right mechanism later, it
arrives as a new `impl` behind a seam that already exists, not by narrowing the
bot's surface. Workstream A still consumes rather than duplicates the similarity
primitive, because that is a leaf calculation with one correct answer; the
distinction is between a leaf that has one answer and a core that must stay
general.

## Design rule: every borrowed part is behind a trait

Nothing below may be used directly by a caller at more than one remove.

| Concept | Seam | Replaced later by |
|---|---|---|
| Similarity metrics | `trait Similarity` | A fitted or learned scorer |
| Free-text intent resolution | `trait Resolver` | A model-backed resolver |
| Flow document encoding | `trait FlowSource` | `braid-flow-ir`'s canonical bytes |
| Session record sink | `trait Journal` | The sealed evidence log |
| Variable interpolation | `trait Interpolate` | `keel::construct::plan_load::interpolate` |

A seam that has exactly one implementation today is still a seam. The point is
that tomorrow's replacement is a new `impl`, not a rewrite.

## Workstream A: `lgwks_std` similarity primitives

New module `crates/lgwks-std/src/similarity.rs`. Pure functions and one trait.
No I/O, no clock, no allocation beyond the input.

Delivered:

- `trait Similarity` with an associated `Value` type and
  `fn score(&self, left: &Self::Value, right: &Self::Value) -> f64`, returning a
  value in `[0.0, 1.0]`.
- `EditDistance`: bounded Levenshtein over normalized strings, normalized to
  `[0,1]` by the longer input length.
- `Jaccard`: set similarity over attribute collections.
- `PathSimilarity`: normalized structural path comparison, discarding volatile
  segments (numeric ids, generated class hashes) before comparison.
- `Geometry`: Euclidean distance over normalized bounding boxes, mapped to
  `[0,1]` by a caller-supplied maximum.
- `Weighted`: a combinator holding `(weight, boxed Similarity)` pairs producing
  `sum(w_i * phi_i)`, with a `threshold` above which the result is accepted.

Requirements:

- `EditDistance` must be bounded: refuse inputs over a declared maximum length
  rather than allocating a quadratic table for a hostile input.
- `Weighted` must be constructible only with a non-empty set of vectors; an
  empty scorer is a `Result` error, not a silent zero.
- Every public item carries a doc comment. The workspace denies `missing_docs`.
- Zero new dependencies. This module uses only `core` and `alloc`.

Acceptance: unit tests per metric including boundary inputs (empty strings,
disjoint sets, identical inputs, maximum length), a test that a `Weighted`
combinator with a single vector returns that vector's score, and a property test
that every metric returns within `[0.0, 1.0]`.

## Workstream B: `lgwks_bot` session and flow runner

New module `crates/lgwks-bot/src/session.rs` plus `crates/lgwks-bot/src/session/`
for its parts. This is a new top-level module. It does not modify
`domain::flow`, which remains the linear composition domain and is a different
job.

Delivered:

- `NodeKind`: `Say { text }`, `Ask { var, options, routes }`,
  `Branch { var, when, then, otherwise }`, `Handoff { target }`,
  `Refer { target, text }`, `Route { dispatch, fallback }`, `End`.
- `FlowSpec`: declared `vars`, an `entry` node id, a node map, and terminals.
  Constructed by `FlowSpec::from_json` (through `lgwks_std::json`, matching the
  `BotSpec` precedent) and validated at construction.
- `VarScope`: declared, named variables, typed as a closed enum of string,
  integer, boolean, and choice. `${name}` interpolation into `Say` and `Refer`
  text, through the `Interpolate` seam. An undeclared name in a template is a
  validation error at load, not an empty substitution at run time.
- `Session`: holds an id, the current `(flow, node)` cursor, the `VarScope`, the
  visited path, and the transcript. `Session::answer`, `Session::current`,
  `Session::transcript`, `Session::terminal`.
- `Terminal`: `Completed`, `Referred { target }`, `HandedOff { target }`,
  `Refused { reason }`. The outcome of a session is this type, never a bare
  string.
- `trait Resolver`: maps a free-text utterance and a set of candidate options to
  `Option<usize>`. The shipped implementation is a weighted keyword scorer using
  `KeywordResolver`, with an explicit unrecognized case that re-asks the same
  node rather than advancing. A model-backed resolver is the intended future
  `impl`.
- `trait Journal`: receives `(path_node, role, text)` records. The shipped
  implementation is an in-memory `Vec`. A durable, sealed implementation is the
  intended future `impl`.
- `flow::validate`: refuses at load, each with a distinct typed error:
  - a transition target that is not a declared node
  - an `Ask` whose option value has no route
  - a declared variable never written
  - a variable read in a template but never declared
  - a node unreachable from `entry`
  - a step count over a declared `budget`
  - an unknown node kind
- A loop guard: a session exceeding `budget` steps returns
  `BotError::SessionBudgetExceeded` rather than looping.

Requirements:

- `Session` fields are private with accessors. The workspace denies
  invariant-breaking `pub`.
- No production `unwrap` or `expect`. Every failure path returns `BotError`.
- No `println!`. The workspace forbids terminal output in library code.
- The runner is synchronous. It drives no I/O and awaits nothing, so it runs
  under `--no-default-features` with no async runtime present. This is
  deliberate: a CDP-driven consumer is synchronous and cannot take the async
  verb surface.

Acceptance: a flow fixture of at least twenty nodes exercising every `NodeKind`,
a scripted session that reaches each terminal kind, one test per validation
error above, a test that an unrecognized answer re-asks rather than advancing,
and a test that a cycle exceeding the budget returns
`SessionBudgetExceeded` rather than hanging.

## Workstream C: `lgwks_deps` invariant register

The workspace already carries an authored, reviewed, refusal-gated register for
dependency edges in `contract/APPROVED.toml`, enforced by `lgwks-deps check`.
This workstream adds a second register of the same shape for invariants, rather
than inventing a second mechanism.

Delivered:

- `contract/INVARIANTS.toml`: a register of declared invariants. Each entry
  carries `id`, `statement`, `scope` (which crate or module it binds), `owner`,
  `enforced_by` (a test path or a lint name), `approved_by`, `approved_on`, and
  `review`.
- A parser and validator for that register in `crates/lgwks-deps`, reusing the
  existing register machinery.
- Refusal rules, each with a distinct diagnostic:
  - an invariant with no `enforced_by`
  - an `enforced_by` naming a path that does not exist
  - an invariant whose `scope` names a crate that is not in the workspace
  - a duplicate `id`
  - a malformed `id` (the `INV-<SCOPE>-<SLUG>` convention already used in this
    workspace)
- A `lgwks-deps invariants [path]` subcommand that reports the register and its
  refusals, exiting non-zero on any refusal.
- `check` gains the invariant register as a second input, so
  `lgwks-deps check .` refuses both an unowned dependency edge and a declared
  invariant with no enforcement.

Requirements:

- The existing `check` behaviour and exit codes do not change for a repository
  with no invariant register. The register is optional; its absence is not a
  failure.
- No new dependencies.

Acceptance: a fixture register per refusal rule, a test that a valid register
passes, a test that a missing register is not a failure, and a test that
`check` reports both registers in one run.

## Out of scope for this stack

Named so they are not silently dropped, and so a later reader knows they were
considered.

- The interface model (element entities, recognition-vector components,
  acquisition systems) on the ECS substrate. It depends on Workstream A landing
  first.
- A synchronous path for the four verb traits. It is an API decision for the
  maintainer, not an implementation task, because it touches the published
  surface of `lgwks_bot` and the one-path policy.
- Durable or resumable sessions. The `Journal` seam is the insertion point.
- Any model-backed resolver.
- Any change to `xpress-runner`, which is out of scope for this workspace
  entirely.

## Constraints binding all three workstreams

- Do not bump any crate version. Do not edit `CHANGELOG.md`. Release is a
  separate step.
- Do not edit the workspace `Cargo.toml` and do not add a dependency.
- Every `#[allow(..)]` and `#[expect(..)]` carries `reason = "..."`.
- A crate that does not compile is a refused change. `cargo clippy
  --workspace --all-targets --locked -- -D warnings` and
  `cargo fmt --all -- --check` must both be clean.
- Every public item is documented. `missing_docs` is denied workspace-wide.

## Corrections from the frontier sweep

The three workstreams above were specified before the literature sweep in
[`frontier.md`](frontier.md) completed. They remain the right shape, and two of
them are now under-specified. These deltas are recorded rather than silently
folded in, because the workstreams were built against the original text.

**Workstream A is a recall stage, not a complete resolver.** Similo's measured
88.0% on real drifted pages is the deterministic default path; a learned reranker
over the top-N takes failures from 70 to 39. So the module is correctly scoped as
written — but it must not be presented as the answer to element resolution. Its
output feeds a cascade.

**Workstream B's validator implements one of three canonical soundness
properties.** "A node unreachable from `entry`" is *no-dead-transitions*. The two
that catch a genuinely malformed flow, and that the acceptance criteria do not
name, are:

- **option-to-complete** — every reachable state can still reach a terminal
- **proper-completion** — the terminal state is the *only* reachable terminal
  state, so a flow able to reach `End` while another branch is live is refused

**Workstream B's resolver needs a posterior and a margin, not a bare
`Option<usize>`.** The measured, principled rule is three-way: above an accept
threshold, advance; ambiguous, re-ask the same node; below an abstain threshold,
terminate. The re-ask counter is per-node and bounded, and the repair **escalates
in form** — verbatim repeat, then narrowed options or explicit confirmation of the
top hypothesis, then terminal — because recovery rates fall off after about the
second repair.

**Two terminal outcomes need a distinct source.** `Refused` must be reachable from
a scope predicate evaluated *before* resolution, and `HandedOff` must carry the
partial evidence and the named unverified constraint. Handing a task back as
though the bot had never been involved produces a measurably worse human error
profile (Jabbour et al., N=259), so a bare target is not sufficient.

**Workstream C's register needs exactly two enforcement kinds** — `static-check`
and `monitor` — and must refuse any invariant whose enforcement is review or
intent. It must also **reject non-monitorable constraints at load**: a
`G(r → F a)`-shaped property has no good or bad finite prefix and can only ever be
unverified. Decidability holds only while the predicate language stays closed over
scalars with a **bounded domain**; an invariant quantifying over unbounded runtime
values cannot be gated at all, and the register degrades to documentation.

**Deferred, and now the highest-value work in the sequence:** the element model.
It is named out of scope above, and the sweep confirms it is the correct next
step, with one addition — the resolver returns a **typed three-way**
(`Resolved` / `Ambiguous` / `Absent`) and the element reference is minted *only*
from the resolved case, so a healed or guessed element has no type-level path into
execution. The moved-versus-gone distinction provably cannot be a scalar
threshold, so this is not a stylistic choice.

## Evidence required before merge

For each workstream: the test command and its result, the clippy result, the fmt
result, and the exact list of files changed. No claim without the command that
produced it.
