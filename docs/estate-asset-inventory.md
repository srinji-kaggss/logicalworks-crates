# Adjacent asset inventory

Status: reference. Companion to [`guidance-runner-spec.md`](guidance-runner-spec.md).

## Why this exists

The guidance runner described in the companion specification needs six
capabilities. Before writing any of them, the surrounding repositories were
searched for an existing implementation, so that the workspace ports rather than
reinvents, and so that a later reader can see what was considered and rejected.

Method: a cross-repository graph query over the shared concept index, structural
queries against this workspace, and a targeted sweep of sibling repositories for
module documentation matching the six capability terms. Third-party vendored
code was excluded.

The six capabilities searched for: flow graph as data; session state; evidence
journal; declarative document compilation; element and selector resolution;
invariant registers.

## Bring in now

Ranked. Each of these is licensed compatibly, small in dependency closure, and
closer to the requirement than anything in this workspace.

### 1. `rocco-runtime` (repo `Rocco`, Apache-2.0)

The lowest invariant, made executable: a deterministic tick, a bounded journal,
an admission and authorization gate, an effect lifecycle, and replay. Three
externally falsifiable invariants are declared:

- **The journal is authoritative.** State is a fold over recorded facts, and
  `runtime::replay` rebuilds a projection from a journal alone, consulting no
  live state.
- **The journal is bounded.** The append that would exceed the ceiling is
  refused, and the refusal is backpressure rather than loss.
- **Authorization separates two clocks.** A pure reducer records the decision
  with the intent; a `RevocationFence` carries live freshness, on the stated
  ground that a recorded decision from yesterday is exactly the evidence that
  must not be trusted today.

Dependency closure is `rocco-contracts` plus `lgwks_std`, both already in this
organization. No network, no database, no async runtime.

**Consequence for the specification:** the interim `Journal` seam, backed by an
in-memory vector, is weaker than this. Journal-authoritative replay is the
correct target and should replace the interim implementation rather than sit
beside it.

### 2. `xpress-runner` content store and write-ahead log (MIT)

A content-addressed store in which an artifact's identity **is** the BLAKE3 of
its bytes, paired with a checksummed append-only log. The log's discipline is
the point: a browser action is announced in it *before* the driver is called,
and the outcome is appended *before* any in-memory projection or observer may
announce it. A clean incomplete log therefore means "outcome unknown", never
"the field was not mapped".

That is the evidence invariant this specification wants, already running.

### 3. `xpress-runner` browser session, guard, and access modules (MIT)

Three separable seams:

- `session.rs` classifies where a session is and what the runner may do about
  it, and keeps classification separate from action. It names the exact URL a
  human should open and types no credential.
- `guard.rs` is a fail-closed authority recogniser. It replaced a substring host
  check that accepted a hostile URL containing the permitted host as a query
  parameter. It recognises exactly one shape and refuses explicit ports,
  userinfo, percent-escapes, backslashes, non-ASCII, and control characters
  rather than normalising, choosing the direction that fails safe.
- `access.rs` models the signed-in user's access level as the scope they may
  operate in, enforced separately from any form field.

### 4. `lgwks-algorithms` graph and dedupe modules (MIT OR Apache-2.0)

Dependency-free building blocks: path-halved union-find with union by size,
minimum-spanning-forest construction, a brute-force k-nearest-neighbour graph in
compressed sparse row form with symmetrisation policies, SimHash random
projection, and MinHash near-duplicate detection.

`KnnGraph` computes cosine similarity and Euclidean distance, which is precisely
the scoring ground the companion specification's similarity workstream covers.
The estate already has this; the specification should consume it rather than
grow a second implementation.

### 5. `lgwks-algorithms` declarative module (MIT OR Apache-2.0)

Encapsulates algorithms in pure pipelines over `(Security, Capability,
Justification)` tuples, and states the boundary explicitly: the values are
diagnostic metadata, not authority, so `execute_declarative` refuses the critical
execution level unconditionally. This is the cleanest existing statement in the
organization of an invariant register whose refusal is unconditional rather than
configurable, which is the shape the companion specification's invariant register
should copy.

### 6. `forge-md-runtime` (repo `forge-harness`, MIT, archived)

A durable memory lifecycle runtime, and the strongest match in the organization
for the model-facing half of this work. It is substantially more than a journal:
it owns the deterministic part of a task lifecycle, journals the exact
task and transcript bytes, derives a source-addressable episode record, and
builds a bounded context snapshot before any successor runs.

The property that matters most: **model-generated candidates stay advisory.**
Candidates are proposed, evaluated, and quarantined through distinct operations,
and only a mechanically derived record is promoted. A model's claim is never
itself the record.

Other properties worth taking:

- A bounded context snapshot with an explicit token budget, rather than an
  unbounded history.
- A retention policy with an enforced eviction path.
- An HMAC-authenticated append-only event log with an issuer identity, so a
  recorded event carries who wrote it.
- A context envelope whose opening marker states that the historical material
  inside is escaped untrusted reference data and not instructions, permissions,
  or authority. That is a prompt-injection boundary expressed as a first-class
  constant rather than a convention.

This repository is **archived and MIT licensed** (`sdk/LICENSE`; the GitHub
repository metadata reports `mit`). An earlier pass recorded it as unlicensed
because it read the crate manifests, which declare no `license` field, and did
not look for the file. That was wrong.

### 7. `forge-md-store` (repo `forge-harness`, MIT, archived)

Reducer is the sole owner of lifecycle semantics; the store supplies a
transactional append-only tape, and replay through the reducer decides whether
that tape is coherent. There are deliberately no model calls, no clocks, and no
prompt rendering in it.

Cost to weigh: it pulls a bundled SQLite build, which compiles the database from
source and is the heaviest single dependency in this inventory. It is the reason
this is ranked below `forge-md-runtime` rather than beside it.

## Document as future

Recorded now so the design does not preclude them.

| Asset | Repository | Why deferred |
|---|---|---|
| `keel` gates | `keel` | A mature pure-policy / effectful-adapter split applied across a family of refusal gates, with typed verdicts and effects exiled to an adapter. Transferable as a pattern; the build closure is large and the license needs verification first |
| `wwfd/comply` | `wwfd` | The only shipped refusal gate over prose, requiring evidence to outnumber inference two to one. **Proprietary**, so at most the rule may be restated, never the code |
| `moo-core`, `moo-text`, `moo-blocks` | `moo` | Signed event log, CRDT text layer, and a three-way replay equality check that a bounded replay equals a full fold equals the stored relation. The equality check is the most valuable idea here for a resumable session. Heavy closure: bundled SQLite, an elliptic-curve signature stack, and a pinned cross-repository git dependency |
| `logical-db-episode` | `logical-DB` | Bounded local episode model with fail-closed handling of unknown versions. Apache-2.0. It is itself a port of a model in `LOGICAL-EVENT-MEMORY`, so read the original first |
| `wm-core` | `cairn` and `world-model` | Content addressed by BLAKE3-256, byte-identical to the digest the flow IR uses, with an explicit gate forbidding it from defining its own `Cid`. The naming discipline is the precedent worth inheriting. Note the crate name is taken on the public registry by an unrelated project |

## Nothing found

Two capability areas have no existing implementation anywhere in the
organization, and one is a deliberate exclusion.

- **Element and selector resolution.** No dedicated resolver exists. The only
  real selector work is a set of per-page field maps welded to one specific web
  application and one browser driver. This must be written fresh. What is worth
  taking is the *verdict discipline* rather than the mechanism: a step resolves
  to verified, to unverified, to failed, or to fatal, where unverified carries
  both sides of the uncertainty, and a run passes only when every verdict is
  verified. That discipline exists because a report could not previously
  distinguish "the value reached the form" from "the selector matched nothing".
- **Declarative document compilation.** Nothing beyond the known reference in
  `keel`. There is no second canonicalise-then-validate pipeline.
- **Flow graph as data.** Nothing beyond the known flow IR. A set of four
  `maps-*` crates were examined and excluded: all four declare in their own
  module documentation that they are skeletons emitting no route, no guidance,
  and no runtime behaviour, and they are under a licence this workspace does not
  use.

## Cross-cutting notes

**One hash, not two.** The organization already shares a single hashing
convention: BLAKE3-256 through this workspace's `lgwks_std` hash feature, whose
bytes are identical to the flow IR's content identifier. Four separate
repositories bind to it, and one of them explicitly forbids itself from
redefining the type. Any journal or evidence layer added here must use the same
bytes rather than introducing a second content hash, because two answers to
"what is this artifact" make a provenance receipt impossible to check against
itself.

**Confirm canonical copies before citing.** Two repository pairs are near
duplicates of each other: `cairn` and `world-model` hold the same core crates,
and `keel` exists in several working copies. Establish which is canonical before
any of them is named in workspace documentation.

**One candidate is blocked on licence.** `wwfd/comply` is proprietary, so at most
its rule may be restated and never its code. `forge-harness` was recorded here as
unlicensed in a first pass and is in fact MIT; the correction is above. Verify a
licence by looking for a licence file and the repository metadata, not only by
reading crate manifests, which frequently omit the field.

## What this changes

The specification assumed the session and journal layer had to be written from
nothing. That is half wrong, and the correction has two parts.

`rocco-runtime` already carries the deterministic tick, the bounded journal, the
authorization gate, and replay, under an Apache-2.0 licence with an
already-compatible dependency closure. `lgwks-algorithms` already carries the
similarity scoring primitive. And `forge-md-runtime` carries the model-facing
lifecycle: advisory candidates, mechanical promotion, provenance, a bounded
context snapshot, and an untrusted-data envelope around historical material.

Together those cover capability areas two, three, and five. The in-flight
workstreams are therefore the parts that genuinely do not exist elsewhere: the
flow document, its validation, and the invariant register.

The remaining gap, and the one no asset in this inventory closes, is element and
selector resolution.
