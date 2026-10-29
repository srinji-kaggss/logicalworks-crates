# Decisions and unresolved obligations

Direction is not implementation acceptance. `user-direction` records the Director's instruction; `proposed` records an engineering recommendation; `blocked` requires evidence before runtime implementation. None means a runtime test passed.

## Decision ledger

| ID | Status | Decision | Rationale / rejected alternative |
| --- | --- | --- | --- |
| D01 | user-direction | Frontend is orchestration; creator vision is primary | Reject a renderer-centric or GPU-benchmark-centric product |
| D02 | user-direction | Rust-first authoring with permitted JS/Wasm glue and hybrid HTML/GPU presentation | Amended by Q01/Q02 and explicit glue approval below; do not require authors to maintain a second application in JavaScript |
| D03 | user-direction | GPUI is the presentation foundation | GPUI is Apache-2.0; rebuilding it to avoid GPL was based on a false premise |
| D04 | user-direction | logicalworks-crates is the primary dependency surface | Reuse owned primitives; admit optional third-party engines through the storefront |
| D05 | proposed | One host-owned authoritative application state; Drishti owns projections and coordination | LOK remains the constellation owner; standalone applications need not run the entire constellation |
| D06 | proposed | One public package with internal ownership modules | Reject a crate per concept and empty scaffold modules |
| D07 | user-direction | Known-dependency scheduling plus adaptive bounded speculation by default when qualified | Q29 supersedes opt-in-only direction; prediction cannot authorize effects; measured admission and pressure shutdown remain mandatory |
| D08 | proposed | Script engines are optional lowering adapters, not foundational application state or policy | Do not replace JS callback sprawl with Rhai/Lua callback sprawl; no new language/VM selected |
| D09 | proposed | Experience-as-Code supplies doctrine/contracts and later independent evaluation | Do not fork its whole runtime or rely on its currently simulated browser provider |
| D10 | user-direction | Keel eventually wired; strict obligations encoded now | Broken tooling is blocked evidence, not grounds to lower standards or report green |
| D11 | user-direction | Repository/product name Drishti | Documentation-only 0.0.1 publication explicitly authorized; no fake runtime to reserve a name |
| D12 | user-direction | Publish design package under Apache-2.0 | Public Cargo design publication approved; GitHub private by communicated default |
| D13 | proposed | Native-trusted and confined-guest profiles have separate guarantees | Rust memory safety and script API restrictions do not prove hostile-code isolation |
| D14 | proposed | Human, accessibility, and agent interactions share scoped semantics | An agent is not a privileged alternate product; visual tasks still need visual proof |
| D15 | user-direction | The Logical Works brand is drishti's default presentation language; the ink lab and GPU residency are the first realized reference core | Director, 2026-09-30: "I want drishti to feel like this as the gpu accelerated runtime and the inklab. Spec is in js but drishti remains the rust framework as specced out in repo." The JavaScript engine (S22) is the behavioural source and drishti stays the Rust framework; this supersedes the earlier documentation-only runtime prohibition for the presentation seats only |
| D16 | user-direction | A working native ink lab over the estate: GPUI admitted through `lgwks_deps`'s `gpui` storefront feature behind drishti's optional `ui` feature | Director, 2026-10-05: "I need a fully working version of this based on our estate". The default build stays dependency-free. A direct `gpui` or `wgpu` edge and a forked renderer were rejected |

## Source conflicts resolved for this design

| Source tension | Resolution |
| --- | --- |
| Earlier bespoke/wgpu-only design versus GPUI adoption | GPUI reuse supersedes bespoke UI; shared compute/render semantics remain required and unproved |
| Old Canvas local stores versus current LOK charter | Current charter/ADR-101 owns constellation durable state; do not copy old SwiftData mirroring as new authority |
| Browser repo's agent-first vs human-browser product directions | Borrow stable semantic interaction and mediated effects, not its product exclusions or human-UI deferral |
| Earlier strict no-JS direction versus later explicit glue and hybrid approval | Later Director answers supersede the strict loader prohibition; application authoring remains Rust-first and browser integration remains unproved |
| Game-research focus/LOD proposals versus accepted implementation | ADR-099 parks focus-distance and LOD research; do not claim a solved intent metric or adopt untested salience heuristics as universal |
| EAC schemas/docs/provider status differ | Reuse truth separation, map native journeys explicitly; no schema-conformance or real-browser-pass claim from prose |
| Old Keel floors and commands versus current broken checkout | Preserve source/version provenance and strict intent; do not copy stale CLI invocations or advisory metrics as mandatory executable gates |
| Estate prose versus current feature/re-export implementation | Read actual consumer API before dependency integration; the design does not invent missing storefront exports |
| Brand's JavaScript engine versus the Rust framework scope | The brand's `lw-ink.js` is the behavioural specification only. Drishti re-implements it in Rust and does not adopt JavaScript as a runtime, copy the engine as a dependency, or change the framework's ownership contracts to match a web page |

## Blocking proof obligations

These are required research/engineering outcomes, not background TODOs. `blocked` means no runtime acceptance for the affected profile.

| ID | Status | Owner seat | Evidence required to close |
| --- | --- | --- | --- |
| G01 | blocked | hosts/browser.rs | Real hybrid HTML/GPU host with permitted glue, useful pre-Wasm HTML/forms, unified island interaction, wide desktop/mobile browser matrix and declared no-GPU compatibility mode |
| G02 | blocked | presentation/gpui.rs | Pinned usable GPUI and estate facade versions; macro/import compatibility; UI executor lifecycle; native and web package availability; complete license/dependency closure |
| G03 | blocked | presentation/residency.rs | Supported shared device/resource synchronization or an explicitly costed interop boundary; zero-hidden-transfer oracle on real compute-to-render work |
| G04 | blocked | hosts/isolation.rs | Selected isolation mechanism with host-call bounds, adversarial escape/exfiltration tests, cancellation, and no native fallback |
| G05 | blocked | scripting/boundary.rs | Decide whether a script adapter is needed; compare Rhai/Lua/Luau against typed Rust declarations on actual authoring, reload, resource, and security workloads |
| G06 | blocked | operations/lifecycle.rs | Transition model covering local atomic adoption, remote outcome uncertainty, duplicates, authority revocation, and code/state version migration |
| G07 | blocked | execution/budgets.rs | Per-profile numeric budgets, calibrated counters/timers, fairness, saturation, protected-state eviction, and worst-case dependency/parse costs |
| G08 | blocked | presentation/interaction.rs | Real keyboard, screen-reader, multilingual text/IME, selection, focus, interrupted motion, and persistent context journeys |
| G09 | blocked | assurance integration | Restored pinned Keel executable and consumer contract; complete required-check wiring and negative controls; independent runtime/experience evaluators |
| G10 | blocked | authoring/declarations.rs | Demonstrated lower coordination burden than idiomatic Dioxus/SwiftUI/React/direct GPUI without losing domain expressiveness |
| G11 | blocked | hosts/lok.rs | Current host-owned snapshot/action contract and constellation type bindings; no duplicate durable authority; independent standalone-host semantics |
| G12 | blocked | hosts/application.rs | Recovery storage, finite retention and exhaustion policy; dependency trust admission; confined-result information-flow boundary; justification/outcome enforcement and confirmation-policy reconciliation |
| G13 | blocked | presentation/interaction.rs | Shared phone/desktop UI workflows, genuine capability restriction classification, offload/skip declarations, automatic adaptation and coherent renderer switching |
| G14 | blocked | diagnostics/causality.rs | Bounded source-linked cause evidence and compact agent semantics; measured diagnostic time and egress savings without fabricated root causes or hidden data collection |
| G15 | blocked | presentation/residency.rs | A real wgpu/WebGPU backend admitted through `lgwks_deps`; a device capture comparing the modelled `BYTES_PER_SHAPE` transfers with measured ones; a supported shared device/queue residency contract; and a driver-loss replay. The deterministic model in `src/runtime.rs` is not this proof |

These gaps do not block publishing this design as a clearly labeled documentation package. They do block pretending the planned runtime is ready to implement wholesale or ship. Subsequent work must close the prerequisites of a narrowly specified executable slice without redefining the product as that slice.

## Sequence by proof, not by crate count

1. Freeze product requirements and counterexamples; review authoring examples in both the immersive fixture and unrelated forms/editor applications.
2. Resolve host/GPUI/estate dependency seams and supported isolation/browser domains. Measure feasibility rather than invent adapter signatures.
3. Formalize admitted operations, generations, budgets, and recovery, then test a pure reference transition model against independent cases.
4. Integrate a real native GPUI journey plus its full failure and accessibility obligations; do not count it as browser evidence.
5. Prove compute/render interop and measured demand scheduling. Add speculation only when matched workloads justify its cost.
6. Admit a script adapter only after its use case, sandbox profile, and ergonomic advantage are established.
7. Qualify every advertised host and restore strict Keel + independent experience release admission. No aggregate green when a required profile remains unknown.

Changes to decisions must name affected requirements, alternatives, evidence, migration consequences, and renewed acceptance cases. Rewriting a sentence to hide a failed proof is not a decision.

The finalized product contract (the 2026-09-12 amendment and its answer ledgers) is in [contract.md](contract.md).
