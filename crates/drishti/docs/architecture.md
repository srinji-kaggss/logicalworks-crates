# Architecture: meaning, work, authority, presentation

Status: proposed technical design. No runtime described here exists in this package.

## Ownership comes before module names

| Concern | Owner | Drishti must not do |
| --- | --- | --- |
| Domain meaning and durable state | Application host; LOK in the constellation | Create a second durable business-state store |
| Canonical principal/capability/effect contracts | Existing authority provider; LOK when integrated | Mint replacement constellation authority envelopes |
| Canonical content identity/IR | Braid when integrated | Invent another Cid encoding or silently reinterpret Braid |
| Cryptographic custody | Selected authority service; Secure Authority when integrated | Hide key custody inside a UI renderer |
| Governed execution receipts | Existing receipt owner; Forge SDK when integrated | Rename an internal event into an authoritative receipt |
| Interaction and derived presentation | Drishti using GPUI and qualified HTML/native islands | Treat component address, traversal index, or screen position as domain identity |
| Execution readiness and leases | Drishti's bounded orchestration layer | Treat task completion order as business ordering |
| Host mechanisms | GPUI and explicit platform adapters | Introduce another business scheduler inside a host callback |
| Experience evaluation | Independent public-surface evaluator | Treat declaration, screenshot creation, or unit tests as journey success |

A generic host contract is not another OS authority. It supplies opaque identities, authorized snapshots, action admission, and operation outcomes. A standalone application may implement that contract; it must disclose which durability and isolation guarantees it provides. The LOK adapter binds existing owners rather than copying their types or algorithms.

## Two connected graphs

The **semantic graph** relates application state, stable subjects, interactions, and derived layout/paint/accessibility/agent views. The **execution graph** relates concrete operations, dependencies, resource versions, transfers, and completion. Lowering connects demanded semantic outputs to executable work.

Do not put accessibility relationships, text shaping, business history, and GPU allocation into one universal tree. They have different lifetimes and traversal rules. Share provenance and identity, not one mutable object model.

```text
creator declarations / admitted human or agent actions
                         |
             authoritative host transition
                         |
               versioned semantic demand
                         |
       dependency derivation and operation admission
                         |
          bounded execution and resource leases
                         |
        validated presentation snapshot through GPUI
                         |
      pixels / input geometry / accessibility / semantics
```

This diagram is a logical relation, not a requirement to serialize every stage. Independent work may overlap after its own prerequisites and admission hold.

## Operation semantics

Each operation binds a subject and generation, input versions, code/schema version, authority scope and epoch, declared effects, deadline, resource budget, and output adoption policy. Cross-domain calls also bind transport and reconciliation semantics. These are proposed contract fields, not a frozen wire ABI.

Three distinct operation categories prevent one convenient but incorrect universal policy:

1. **Derivation:** computes a value from identified inputs. Reuse is permitted only when its dependency/equivalence contract holds.
2. **Observation:** acquires external information with explicit authority, freshness, privacy, and cost. A read is not necessarily pure or free of side effects.
3. **Command:** requests an authorized domain transition or external effect. It is not replayed by render, layout, speculative evaluation, or dependency refresh.

The author supplies domain policy: replace obsolete preview, preserve ordered edits, serialize conflicting commands, or use a declared merge algebra. The runtime derives scheduling and feedback from that policy. Unknown conflicts are not resolved by arrival order or model judgment. The finalized authoring direction is a small declarative Rust macro, tracked reads with an inspectable graph, and intent/function-first operations. Policy is expressed once at its owning boundary, not repeatedly at each widget.

## State machines and commit boundaries

Work moves through proposed -> admitted -> prepared -> running -> result-ready -> adopted, with explicit rejected, failed, expired, cancelled-before-start, draining, and outcome-unknown states. An external command additionally distinguishes authorized intent, dispatch, confirmed external outcome, and host reconciliation. A frame distinguishes validated, submitted, presented where observable, and retired.

These state machines are related but not interchangeable. A task future resolving does not prove a remote write committed. A GPU queue accepting commands does not prove a frame was seen. A cancellation request does not revoke an already dispatched effect.

Before adopting a result, compare its dependency versions, subject generation, authority epoch, freshness contract, and current demand. Validation and local adoption must occur at one serialized host transition or an equivalent atomic compare-and-commit boundary. For remote effects, server-side authorization/idempotency and reconciliation are required; a client check cannot close a remote time-of-check/time-of-use race.

Duplicate delivery is keyed by operation identity and a declared bounded retention horizon. Once an outcome cannot be established, report unknown and reconcile. Do not promise exactly-once external execution through indefinite retries.

## Determinism without pretending the world is deterministic

For a fixed admitted contract, ordered external observations, explicit time/random inputs, and host policy version, state transitions must be replayable. Worker completion timing is not deterministic. Replay records the observations and choices needed by the declared profile; it does not require permanent collection of all user behavior.

Numeric semantics, tolerance, locale, fonts, and backend identity participate where relevant. Cross-platform pixel identity is separate from semantic equivalence. Dynamic dependency discovery is allowed, but partial read sets cannot authorize stale reuse. Iterative/cyclic computations need an explicitly bounded convergence contract; a plain dependency DAG cannot represent arbitrary feedback faithfully.

## Speculation is an optimization over the same semantics

First exploit known dependencies. The finalized product direction enables adaptive bounded speculation by default for qualified eligible work, while retaining a non-speculative mode and disabling admission under pressure. It predicts an operation or prepares an already specified branch while demand is unresolved. Candidates are isolated from authoritative state and visible success until adoption. This default is not active in the documentation package; calibration and matched-workload evidence remain prerequisites.

Admission requires authorized speculative access, known input provenance, acceptable effects, a finite waste budget, and capacity not needed for foreground work. Cache identity includes input versions and authority, not just function name/arguments. Nondeterministic invocations require occurrence identity unless an explicit sharing contract permits reuse.

In the ideal overlap case latency approaches max(decision time, preparation time), rather than their sum. Predictor cost, validation, queue contention, cancelled work, and bandwidth can eliminate the gain. Never claim a miss cannot slow the baseline. Disable or reduce speculative admission under pressure; bound starvation for background work.

Unknown effects cannot be inferred pure from Rust types, an AST, a function name, or a script declaration. Native host calls remain part of the trusted boundary even when invoked by a sandboxed language.

## Presentation and interaction coherence

GPUI owns its supported layout, prepaint, paint, text, and platform machinery. Drishti must integrate with that ownership rather than run a competing frame loop. An integration proof must establish where demands enter, where tasks wake the UI, and how resource completion becomes a state transition.

Logical snapshots connect visible state, hit testing, focus, selection, composition, and semantic exposure. Hosts may acknowledge accessibility and presentation at different times; track those states instead of pretending there is one atomic OS commit. Stale gestures and agent actions must resolve against subject generation and an admitted version, never a replacement control occupying the same pixels.

Animations are interruptible derivations, not business clocks. Reduced-motion mode preserves navigation and action meaning. Accessibility and IME cannot be deferred behind a GPU demo. A screen-reader or keyboard focus can demand content even when it is not visually on screen.

No blind offscreen eviction: pinned drafts, composing text, active effects, assistive focus, and in-flight GPU leases are protected. If all candidates are protected, apply backpressure or named degradation instead of violating a resource or data invariant.

Continued in [architecture-operations.md](architecture-operations.md): the ink lab and GPU residency, resource accounting, security and scripting, hosting, recovery and inspectable intent.
