# Codebase structure and dependency direction

Status: planned runtime structure, not generated skeletons. Only the documentation package and bundle tests exist today.

## Actual publication structure

```text
Cargo.toml                     a logicalworks-crates workspace member (crates/drishti)
README.md / VISION.md / LICENSE / AGENTS.md
src/lib.rs                     Rustdoc design modules + the realized seats below
src/design.rs                  Logical Works tokens, roles, colourways, contrast (R35)
src/ink_lab.rs                 the eleven-effect liquid ink engine's public types (R35, R37)
src/ink_lab/engine.rs          host state, the seeded source and the frame step
src/ink_lab/engine/effects.rs  the per-effect advances
src/ink_lab/engine/beams.rs    the mark's published beams and shared helpers
src/runtime.rs                 GPU residency: demand, budgets, epochs, transfers (R36)
src/native.rs                  native GPUI host for the ink lab, feature `ui` (R09, R36)
src/native/lab.rs              the window, its tiles and controls
src/native/lab/paint.rs        placement and painting of frames
src/bin/drishti.rs             the CLI: tokens, ink frames, residency and limits as JSON
docs/
  architecture.md (+ -operations) ownership, execution semantics, residency and recovery
  codebase.md (+ planned-tree) this sole structure authority
  decisions.md (+ contract)    decisions, open proof obligations, the finalized contract
  research.md                  source findings and comparison basis
  assurance.md                 acceptance and eventual Keel requirements
  status.md (+ status-history) implementation/evidence status
  inklab.md (+ ink-engine)     the brand as the presentation layer (S22)
  gpu-runtime.md (+ native-host) the residency model and its first backend (S22)
contract/requirements.tsv      normative requirement registry
experience/journeys.tsv        acceptance scenarios, all unexecuted
tests/bundle.rs                document inventory/reference integrity only
tests/sim/                     seeded deterministic simulation of the engine and runtime
examples/ink_preview.rs        writes an animated SVG preview of the mark (public-API consumer)
examples/ink_lab.rs            opens the ink lab window and prints a measured session report
```

One public `drishti` package is the planned authoring surface. Internal modules are not separate public crates merely because they have different responsibilities. The 0.0.1 release exported only documentation; the working tree now adds the first realized seats as an explicit API transition. These realized seats are deterministic reference cores with unit and simulation tests, not qualified runtime subsystems: `design` and `ink_lab` realize the planned brand/motion seats, `runtime` realizes the planned `presentation/residency.rs` seat, and `native` is the first slice of the planned `presentation/gpui.rs` seat. The rest of the planned tree below remains **planned / absent**.

The planned runtime tree, a plan and not shipped code, is in [planned-tree.md](planned-tree.md).

## Allowed dependency direction

Authoring produces semantic descriptions and operation requests. Semantics does not import GPUI, networking, a scripting VM, or the scheduler. Operations consume the host authority contract. Execution consumes admitted operations. Presentation consumes snapshots and emits typed interactions. Host integration connects lifecycle and external outcomes without becoming a second state owner. Scripting lowers into the same authoring/operation boundary, never around it.

An observed asynchronous result returns through operations to the host's transition boundary; it does not mutate a widget directly. Diagnostics observes these boundaries under a finite budget. It does not grant authority or change behavior to improve a metric.

## Primary dependency surface

Use `std` first, `lgwks_std` for owned primitives, `lgwks_bot` for admitted async/actor work, and `lgwks_ast` where its supported parser contract is needed. All new third-party dependencies, including GPUI or a scripting engine, go through optional `lgwks_deps` storefront features and the owning approval registry. No direct `tokio`, `serde_json`, `rhai`, `mlua`, or `wgpu` edge is silently introduced in Drishti.

The documentation-only release has zero dependencies; that is not evidence of the future runtime's dependency cost. Runtime manifests must pin actual usable published/source identities and measure complete target feature closures. Do not copy upstream source into this repository as a shortcut around admission.

Observed local logicalworks-crates facts: its `lgwks_deps` manifest exposes a `gpui` feature and depends on GPUI 0.2; it currently enables `gpui/default`. An independent review during this session observed the public `pub use gpui` export under that feature, correcting the earlier source observation made before the sibling changed. Its doctrine also contains older `lgwks_rt` ownership prose while the source now re-exports tokio from the storefront. Export existence does not establish the complete consumer contract: version/macro compatibility, executor ownership, and target closure still require verification. Drishti does not repair the sibling repository as part of this design publication.

## Feature plan, not current Cargo features

Keep the authoring/semantic core independent of presentation and runtime selection. `ui` now exists: it enables `lgwks_deps/gpui` (GPUI 0.2.2 in the lock file) and the `native` module, and nothing else. Remaining candidate features include `compute`, `speculation`, a later justified script adapter, and host-specific bindings. The product default is adaptive bounded speculation when qualified; compile-time packaging and exact default feature sets remain blocked on measured closures and host proof. Do not add empty Cargo feature switches to make this plan appear implemented.

GPUI main currently contains web/wgpu adapter crates, but the admitted GPUI 0.2 dependency does not by itself establish access to those exact sources. Version compatibility, published availability, licenses, macros/re-export compatibility, UI-thread ownership, and GPU synchronization must be verified together.

## Test allocation

The requirement registry maps each obligation to its planned module seat and a named future test file. The journey registry supplies public-surface falsification cases. Runtime implementation begins only after the relevant open design obligations are closed, tests are specified with independent oracles, and strict assurance can report honestly. File existence will never be used as a substitute for executed behavior.

The finalized product contract is maintained in `docs/decisions.md`, not a second plan or API specification. Added requirements for HTML hosting, phone parity, design tokens/controls, default recovery, compact agent semantics and diagnostics bind to the seats above. This allocation is not permission to implement new storage engines, duplicate LOK/Braid authority, or create separate per-platform applications. Cargo remains the baseline workflow; a convenience CLI is optional and not present.
