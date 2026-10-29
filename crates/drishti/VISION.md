# Vision: creativity without coordination debt

Status: product direction formalized on 2026-09-12; runtime unimplemented.

## The product

Drishti makes Rust a desirable frontend authoring medium by taking responsibility for mechanical coordination. The author expresses the experience, the meaning of its state, and the consequences of its actions. The framework determines legal dependencies, work lifetimes, scheduling, reuse, interruption, and presentation without asking the author to continually repair callback ordering.

The promise is not zero thought. Domain meaning cannot be inferred safely: a second purchase may mean another purchase, whereas a second search query usually replaces the first. Authors decide that meaning once. They should not reimplement its lifecycle, pending state, retry discipline, and stale-response protection at every control.

The framework must be understandable by inspection. A short API concealing inconsistent state, ambient authority, or an undocumented scheduler is not ergonomic. The normal path should be the safe, composable path; departures must expose the added obligation and trust boundary.

## Reference experience: Logic OS Canvas

The reference fixture comes from Logic OS's Canvas and game-dynamics documents, interpreted through its current charter and ADR-101:

- **Returning to work restores context.** Reopening a document surfaces the last meaningful work, unresolved decisions, and relevant responses, subject to access and retention policy. It does not invent memories or rely on mandatory inference.
- **Work is visible as a route.** An invoice travels through acquisition, parsing, validation, ledger update, and notification. The person can inspect where it waits, why a step was rejected, and what a proposed change affects.
- **Automation composes from meaningful parts.** People connect typed operations inside their authority rather than scripting privileged implementation details.
- **Navigation preserves continuity.** Immersive movement, interruptible transitions, and stable work identity replace repeated context reconstruction. Reduced-motion navigation achieves equivalent outcomes without motion.
- **Failure belongs in the experience.** A failed operation shows its cause, preserved work, scope, and next safe action in context. A diagnostic code may accompany that explanation; technical truth is not hidden for aesthetics.
- **The working set follows demand.** Dormant content is not continuously animated or recomputed. Evicting a disposable projection must never evict unsaved work, an in-flight lease, or the only recoverable copy of state.

The style is semantic continuity, not a compulsory palette, game engine, simulated physics, world metaphor, or single-person shell. Older Canvas examples involving SwiftData authority, mandatory singleton command bars, generated JavaScript, degraded web choreography, or deferred accessibility do not override the current Drishti requirements.

## Universality, precisely

Drishti must support independently scoped applications, users, workspaces, and instances; standard forms and editors; connected and offline work where the operation permits it; local and remote effects; small and large datasets; different locales, scripts, input methods, and accessibility needs. Applications choose their durable domain-state owner rather than importing a mandatory LOK service.

The invariant is common semantics, not identical mechanisms on every platform. Native, browser, isolated guest, and service integrations must advertise actual capabilities and refuse unmet requirements. A platform without the required host interfaces is unsupported for that profile; it is not made supported by a blank screen, software fallback, or a native demonstration.

The following are not established: universal execution of arbitrary programs, automatic business-conflict resolution, whole-web compatibility, strict hard realtime, bit-identical pixels across GPU/font stacks, or a safe interpreter for unrestricted native callbacks. These are claim limits, not permission to discard a requested user journey.

## Adoption must be earned

The baseline is competent production use of existing systems, not deliberately broken React or verbose Rust:

- Against JavaScript frameworks: remove coordination obligations while preserving deployment, accessibility, debugging, and iteration quality.
- Against Tauri: add coherent application orchestration and the desired presentation model without losing an enforceable trust boundary.
- Against Dioxus: exceed its existing Rust authoring, resource lifetime, reactive dependency, and tooling ergonomics; do not relabel these as inventions.
- Against SwiftUI: make semantic declarations buy consistent behavior across the complete operation lifecycle, with equally serious native interaction and accessibility.
- Against direct GPUI: demonstrate that Drishti reduces author-maintained coordination and failure defects enough to justify its overhead.

Measure author completion time, change time, manually maintained coordination rules, realistic failure defects, diagnostic comprehension, escape-hatch use, build/iteration time, and actual end-user task outcomes. Measure resource costs on those same workloads. No weighted score may compensate for failed authority, accessibility, state integrity, or admitted browser-host requirements.

## Human and agent operation

Humans and agents act through the same admitted operations and scoped state. An agent may propose a route or invoke a granted action; it cannot mint identity, elevate its own permissions, infer consent from a prediction, or claim an effect completed merely because it called a tool.

The model is not the scheduler. Prediction may improve readiness; deterministic policy decides whether predicted work may start and whether its result may be adopted. No agent-only route may bypass what the human can inspect and revoke.

## Normative sources

The finalized questionnaire contract in `docs/decisions.md` makes macOS, Windows, mainstream desktop/mobile browsers, iOS and Android target families. Shared ordinary UI workflows must not require a laptop merely because phone implementation was omitted; the Director's Jira/GitHub example is an acceptance case. Automatic adaptation preserves meaning and native behavior. Only genuine execution/capability constraints permit a declared developer-selected offload or skip; neither authorizes unsanctioned data transfer.

Rust-first authoring allows JS/Wasm glue, visible HTML/native islands and automatic inspectable hybrid rendering. Public content and ordinary forms must work before Wasm or without scripting. GPUI remains the strong default; accelerated fallback is a qualification task, not evidence of universal support.

Safety should feel like ordinary dependable behavior: removing work retains recoverable identity/content; closing a view does not lose a draft. The Director's later need for line 33,221 of a removed file must be satisfied within the declared recovery contract without prior backup ceremony. Finite retention and failure behavior require proof, not silent irreversible deletion.

Agents should discover and perform the same authorized functions with compact semantic data rather than reconstructing sitemaps or writing a separate tool integration. Lower egress, reduced unnecessary computation, intent-first development and diagnosis in seconds are measured product goals, not achieved claims. Ambiguous cause stays explicitly unknown rather than becoming a fabricated explanation.

Requirement IDs and exact obligations live in `contract/requirements.tsv`. The journey registry is the acceptance case inventory, not execution evidence. Source identities and supersession decisions live in the research and decision documents. This vision guides those artifacts; mechanisms that frustrate it must be challenged rather than defended because they already have code.
