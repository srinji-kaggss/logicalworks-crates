# The finalized product contract

## Finalized product contract — 2026-09-12 amendment

Status: **finalized requirements baseline, runtime admission blocked**. Authority: the Director's 40-question responses, batches B1/B1B, subsequent first-principles discussion, and instruction to finish and finalize the contract. This amendment supersedes conflicting earlier product direction in this repository; it does not rewrite the already-published 0.0.1 artifact. S19–S21 identify the additional sources.

The tables preserve answer provenance, not a verbatim chat transcript. Quotation marks mark short verbatim wording; unquoted text is a normalized interpretation. Q is the initial questionnaire; B1 and B1B are the conflict-resolution batches. Later explicit corrections win. A question or challenge is not consent to the assistant's option. Unasked batches are not represented as answered. Technical mechanisms remain engineering obligations, not promises approved by selecting an outcome.

### Initial answer ledger

| ID | Director answer / normalized decision | Consequence and acceptance reference |
| --- | --- | --- |
| Q01 | Hybrid by content | HTML and GPU are complementary; R10/R24, J10/J20 |
| Q02 | Visible native islands | Visible DOM/native controls permitted; unified interaction required, R24 |
| Q03 | "wgpu as fallback" | Preferred candidate, not universal-compatibility evidence; B1.2 defines unavailable-backend behavior |
| Q04 | Full web scope | Public content and standard forms included, R24 |
| Q05 | "Human should not feel a difference, safer than today, faster ideally" | No broken interaction seams; safety mandatory, speed comparative, R20/R24 |
| Q06 | Mainstream desktop and mobile | Versioned real-browser matrix required, R10/R25 |
| Q07 | Native and browser proofs first | Both required before shared API stabilization; not evidence for remaining targets, R33 |
| Q08 | Renderer replaceable | Later GPUI preference constrains escalation; no automatic replacement authority, B1B.6 |
| Q09 | Shared identity, native behavior | Platform conventions without losing product identity, R25/R26 |
| Q10 | Automatic adaptation first | Framework adapts before author overrides; no missing phone UI workflow, R25 |
| Q11 | Stacks, grids, constraints | Typed authoring vocabulary; not a mandate for bespoke layout code, R26 |
| Q12 | Strict design system | Registered tokens and variants rather than uncontrolled per-component style, R26 |
| Q13 | Semantic transitions | Interruptible intent-based motion with equivalent reduced-motion behavior, R02/R26 |
| Q14 | Production controls included | Accessible conventional controls provided; no assumed rich-editor engine commitment, R26 |
| Q15 | Both accessibility layers | Authoring diagnostics plus real release journeys, R02/R26 |
| Q16 | Small declarative macro | Compact Rust surface with useful source diagnostics; syntax still to be demonstrated, R27 |
| Q17 | Tracked reads, visible graph | Automatic dependency capture and inspectable causality, R27 |
| Q18 | "Minimum work, intent and function first" | Do not require repeated lifecycle plumbing; domain ambiguity cannot be guessed, R01/R27 |
| Q19 | Explicit trusted boundary | Ambient native escape hatches are visible; not sandbox guarantees, R12/R30 |
| Q20 | Opinionated default host | Useful standalone host replaceable under the same ownership contract, R04/R28 |
| Q21 | Durable drafts by default | Recovery is out-of-box behavior, not a developer reminder, R13/R28 |
| Q22 | Coordination, not sync engine | Existing/application service owns synchronization and conflict policy, R28 |
| Q23 | Automatic local edit history | Eligible local edits have history; no false undo of external effects, R28 |
| Q24 | Shared semantic destinations | URLs/windows/stacks resolve stable subjects, R25/R28 |
| Q25 | Both declaration and negotiation | Capability requirements plus dynamic loss/revocation handling, R34/J30 |
| Q26 | Confined plugins optional | Untrusted extensions require actual confinement; not every pane or trusted function, R12/R30 |
| Q27 | None until demonstrated | No scripting engine by inertia, R11 |
| Q28 | Same function for agents at a fraction of egress; fast, accurate, intent-based development | Discoverable compact semantics without separate sitemap/API scripting; no measured reduction yet, R14/R31 |
| Q29 | Adaptive bounded default | Qualified speculative preparation enabled adaptively, never new authority, R19 |
| Q30 | Integrated executor adapters | No competing mandatory universal executor, R09/R27 |
| Q31 | Matched workloads plus ceilings | Calibrate numeric budgets against competent implementations, R08/R20 |
| Q32 | State-preserving reload | Preserve compatible work; reject incompatible migrations visibly, R17 |
| Q33 | "Debugging should take seconds"; software should know what broke | Source-linked causal evidence; target needs timed measurement and explicit unknowns, R32 |
| Q34 | Headless contracts only | Applications own failure appearance; framework requires truthful state/recovery handling, R32 |
| Q35 | Both with negative controls | Shared intent, real host tests and demonstrably sensitive failure gates, R18/R33 |
| Q36 | Cargo-first, optional CLI | No mandatory proprietary build/deployment service, R33 |
| Q37 | One canonical vocabulary | One name per concept, consistent units and imports; avoid synonym-heavy APIs, R27 |
| Q38 | Stability before adoption | No production-adoption promise before qualified stable contracts, R33 |
| Q39 | Convenience through reversibility; "brother just think" encoded statically | Later laws-of-physics discussion supersedes prompt-centric interpretation, R29 |
| Q40 | Shared tokens, native text feel | Semantic visual consistency, not identical pixels; native text/IME conventions, R26 |

### Conflict-resolution ledger

| ID | Director answer / correction | Final disposition |
| --- | --- | --- |
| B1.1 | Automatic, inspectable, overridable | Renderer selection follows component semantics/capabilities; supported override preserves meaning |
| B1.2 | "2 BUT we need to target wide compat" | Declared compatibility experience allowed when GPU routes are unavailable; broad compatibility remains required, not an excuse to exclude phones |
| B1.3 | Content and standard forms | Useful HTML navigation and standard form submission before Wasm or without scripting; rich workspace requirements disclosed |
| B1.4 | "everything runs in sandbox" | Scoped by B1B.4: untrusted extensions confined, not a mandatory sandbox around all first-party code |
| B1.5 | Unified behavior across boundaries | Focus, input, selection, overlays, accessibility and supported dragging must remain coherent |
| B1.6 | End-user access and performance; laptop sentence later corrected | Phone functionality is mandatory; renderer-switch state policy below is an engineering derivation, not a selected option |
| B1.7 | GPUI strongly preferred; accelerated universal fallback desired | Reuse GPUI; qualification of web/mobile seams and fallback remains G01/G02/G13 |
| B1.8 | Deployment up to developer; least unnecessary compute/egress | Support deployment choices through explicit host contracts; do not silently choose a mandatory server |
| B1B.1 | "NOT Need my laptop"; Jira/GitHub missing phone UI counterexamples | Shared ordinary UI workflows cannot be omitted for screen size or implementation convenience |
| B1B.2 | "Dev chooses between Server Offload vs Skip" | Applies only to genuine execution/capability constraints, not lazy UI omissions; offload still obeys data/authority policy |
| B1B.3 | Hard authoring constraints | Reject undeclared workflow omissions; runtime/device evidence still needed to substantiate coverage |
| B1B.4 | Untrusted extensions confined | First-party native code trusted at this layer; isolation claims apply to actual confined profiles |
| B1B.5 | Requested a spectrum, then rejected permission-centric framing | No selection among proposed grant menus; use structural operation semantics, with residual policy questions explicit |
| B1B.6 | "Hardware acceleration, what do you think wont work?" | No alternate renderer or fork authorized by this question; reproduce a concrete incompatibility and seek a decision |

### Structural safety and state contract

The Director's model classifies execution as internal state/resource changes and ingress/egress across a defined boundary. Internal state includes registers, memory, ownership and execution resources over time. Lazy demand does not prove termination. Engineering enforcement must bound private allocation, execution and host-call work and support actual interruption; it must not claim to solve general halting.

First-party means precompiled code authored by the Director or their coding agent, inherently trusted **at this layer**, with defense in depth elsewhere. Third-party computation has no independent external-effect authority. Authorship and artifact admission of transitive dependencies, dynamic code and updates remain G12; compiling a library is not silently treated as Director approval. Whether and how a confined result may influence a later trusted output needs an explicit information-flow contract before that profile is admitted. No direct or indirect egress bypass may be invented by calling it a result.

Safety is structural and normally unobtrusive, not a moral classifier or repeated permission negotiation. A normal removal operation retains recoverable content and identity; a later request for line 33,221 must retrieve the retained revision or restore the file. Hiding bytes in an undiscoverable trash folder is insufficient. View closure does not discard drafts; replacement preserves relevant history; moves preserve stable references. These are requirements for framework-mediated operations, not a claim to intercept arbitrary native syscalls or repair an external permanent deletion.

Recovery is not infinite storage. Retention horizon, exhaustion, corruption, key loss and final purge authority remain G12. No silent eviction of the only recoverable copy, fabricated durability, or substitution of permanent deletion is permitted. External disclosure is not undone by retaining a local revision. Developer-selected offload cannot bypass locality or create consent. Normal admitted operations should proceed without redundant prompts; unsettled external-effect rules cannot be inferred from this convenience goal.

Use the existing LOK authority boundary and Braid content/vocabulary contracts rather than minting another authority. ADR-062 proposes `F(Identity, Capability, Intent, Outcome)` and a semantic reason; its proposal status is not full enforcement evidence. The inspected typed authorizer and Braid attenuation verifier cover related concrete checks, not the complete equation. The separate LOK confirmation-risk gate must be reconciled with this product experience before claiming an integrated frictionless path. Standalone applications retain a replaceable host contract without mandatory LOK deployment.

### Derived engineering constraints — not additional Director answers

To satisfy durable drafts, semantic destinations and unified interaction, renderer/layout switching must preserve subject identity, drafts, edit history and admitted operations, and reconcile focus/selection without losing composition. A switch may be deferred during composition rather than discard input. This is derived from R02/R17/R24/R25/R28; implementation must falsify it on real hosts.

Imperative domain steps publish coherent versioned snapshots to reactive observers. A local commit must not expose half an invariant. An asynchronous remote operation is not one magically atomic transaction across `await`; it has explicit pending, failure and reconciliation states. Track actual dynamic reads; separate pure derivation from commands; retain bounded handling for cycles. The supplied semantic-borrow-checking brainstorm motivates comparison, not adoption of a new compiler, raw-pointer graph or theorem prover.

Default controls and strict design tokens do not impose branded error widgets: production controls expose headless operation states; applications own failure appearance. Static diagnostics require handling of failure/denial/unknown outcomes without prescribing one visual design. Automatic adaptation and low-friction authoring must expose effective policy and source-linked causality without requiring users to learn runtime internals.

### Finalization, proof and change control

Requirements and journeys remain canonical in their TSV registries; all are unmeasured. R24–R34/J20–J30 extend the original cases, and R35–R37/J31–J33 add the brand ink lab and GPU residency model (D15) without claiming a device. The framework must demonstrate native and browser workflows before shared API stabilization, then qualify every advertised target. Numeric resource ceilings, diagnostic-seconds thresholds, agent egress ratios, renderer versions, macro syntax, trust admission, retention and information-flow policy are not silently decided here; their affected runtime profiles remain blocked under G01–G14.

For a disputed future behavior: cite the answer ID, its later correction, requirement and acceptance case. If the behavior was not selected or contradicts the case, it is an engineering defect or open decision, not the Director's fault. This record is traceability, not a waiver of responsibility. Any change to a settled outcome requires the affected IDs, reason, counterexamples, trade-off, migration effect and renewed Director approval. Finalizing this document authorizes neither runtime implementation nor another public release.
