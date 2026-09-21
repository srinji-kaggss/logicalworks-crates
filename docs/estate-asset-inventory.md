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

## Document as future

Recorded now so the design does not preclude them.

| Asset | Repository | Why deferred |
|---|---|---|
| `forge-md-runtime` | `forge-harness` | Closest match to "session transcript plus resumability plus content-addressed episode", and its rule that model-generated candidates stay advisory while only mechanically derived records are promoted is worth copying. **Declares no license**, which blocks ingestion into this workspace |
| `forge-md-store` | `forge-harness` | Reducer-is-sole-authority with replay deciding tape coherence. Same license blocker, and it pulls a bundled SQLite build, which is a real cost against this workspace's dependency posture |
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

**Licence blocks three strong candidates.** `forge-harness` declares no licence
at all and `wwfd` is proprietary. Those two are excluded on licensing, not on
merit, and both would otherwise rank near the top. That is a decision for the
owner, and until it is made neither can be ingested.

## What this changes

The specification assumed the session and journal layer had to be written from
nothing. That is half wrong. `rocco-runtime` already carries the
journal-authoritative, bounded, replayable session with an authorization gate,
under an Apache-2.0 licence with an already-compatible dependency closure, and
`lgwks-algorithms` already carries the similarity scoring primitive. The
in-flight workstreams are therefore the parts that genuinely do not exist
elsewhere: the flow document, its validation, and the invariant register.
