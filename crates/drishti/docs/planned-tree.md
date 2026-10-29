# The planned runtime source tree

## Planned runtime source tree

Every path below is **planned / absent**. These are ownership seats, not stub capabilities.

```text
src/
  lib.rs
  authoring/
    mod.rs                      coherent user-facing composition surface
    declarations.rs             experiences, derived values, operation policies
    diagnostics.rs              source spans, causes, recovery explanations
  semantics/
    mod.rs
    identity.rs                 opaque host identity bindings + local generations
    dependencies.rs             versioned read sets and invalidation
    transitions.rs              admitted local transition rules
    snapshots.rs                immutable derived presentation inputs
  operations/
    mod.rs
    admission.rs                host-authority adapter; never a second authority
    lifecycle.rs                preparation/result/adoption/failure states
    conflicts.rs                declared ordering and merge-policy enforcement
    reconciliation.rs           duplicates, remote unknown outcomes, retry policy
  execution/
    mod.rs
    demand.rs                   demanded closure and replaceability
    planner.rs                  legal execution lowering
    scheduler.rs                bounded readiness, priority, fairness
    budgets.rs                  checked reservations and high-water accounting
    leases.rs                   scoped CPU/GPU lifetime and draining
    speculation.rs              adaptive bounded candidate isolation and adoption
  presentation/
    mod.rs
    gpui.rs                     GPUI integration boundary with qualified islands
    interaction.rs              action targets, focus, selection, composition
    semantics.rs                accessibility/agent projections, not domain truth
    motion.rs                   interruptible motion + equivalent reduced motion
    residency.rs                optional explicit compute/render interop
  hosts/
    mod.rs
    application.rs              replaceable default host + authority/state contract
    native.rs                   platform lifecycle through supported GPUI seams
    browser.rs                  hybrid HTML/GPU integration with permitted glue
    lok.rs                      optional binding to existing LOK contracts
    isolation.rs                observed guest confinement; no native fallback
  scripting/
    mod.rs
    boundary.rs                 one versioned guest/host contract
    rhai.rs                     candidate only, not selected dependency
    lua.rs                      candidate only; Lua/Luau profile distinguished
  diagnostics/
    mod.rs
    causality.rs                why started/blocked/invalidated/adopted
    redaction.rs                bounded, scoped evidence publication
    replay.rs                   deterministic observations, not effect reissue
tests/runtime/                  independent invariant and negative-control tests
tests/experience/               real public human/agent journey tests
tests/conformance/              host/schema/version-skew equivalence
tests/faults/                   interruption, loss, corruption, partial failure
benches/                       matched direct-GPUI/idiomatic-comparator workloads
examples/                      complete applications, not fake success demos
```

Before introducing the runtime paths, decide whether each source seat is genuinely needed. A planned module may merge with an adjacent owner through a recorded decision; requirements and tests must not disappear with the file. The scripting adapter alternatives must not both be implemented by inertia.

