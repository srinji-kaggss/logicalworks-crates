# Architecture, continued: residency, resources, security and recovery

## The ink lab and GPU residency

The presentation foundation is GPUI plus qualified HTML/native islands. Sitting
inside that boundary, two realized seats carry the Director's Logical Works
brand (`docs/inklab.md`, `docs/gpu-runtime.md`, source S22):

* **The ink lab** (`src/design.rs`, `src/ink_lab.rs`) derives frame geometry and
  goo-filter parameters for the eleven brand effects. It owns the adaptive
  quality tier, the per-host stride, off-screen suspension and the
  reduced-motion resolution. It does not own a device, a frame loop, or any
  business state; a frame is a derived value that is discarded.
* **The GPU residency model** (`src/runtime.rs`) owns demand-driven submission,
  device epochs and leases, upload reuse, transfer accounting and enforced
  budgets. It is the only supported way to present an ink frame, and it refuses
  an over-budget frame with a typed error rather than overcommitting a device.

This is a deliberate narrowing of the presentation seat, not a new authority.
The residency model consumes the ink lab's derived frames and produces
receipts; it never reads domain state, so it cannot become a second state
owner. The accelerated backend itself remains blocked (G03): the model is
deterministic and headless because a real wgpu edge, shared device/queue
contract, and driver capture are all still unproved. Animation stays an
interruptible derivation rather than a business clock, exactly as the
presentation section above requires; reduced motion resolves every effect to
its finished frame.

## Resource accounting

Every shipping profile declares finite input bytes, nodes/edges, dynamic dependency growth, text/glyph work, parser depth, execution fuel, wall time, host-call cost, concurrency, queue depth, cache bytes, GPU bytes, retry count, and evidence retention. Limit values remain open until calibrated; an unspecified bound blocks runtime admission.

Reserve before proportional allocation or submission; checked arithmetic precedes multiplication. Account for host allocations and transfers, not just interpreter/driver counters. Cancelled GPU work keeps its leases until completion or verified device-loss retirement.

For N nodes and E edges, full dependency validation has an O(N+E) target; changed-demand processing is proportional to the affected subgraph plus indexing/scheduling cost, not magically O(1). Global layout may invalidate the whole scene. Bounded heaps may add logarithmic scheduling costs. Explicit budgets must bound adversarial full invalidation and scripts that continually discover new work.

## Security and scripting

Trusted native code and confined guest code are separate profiles. First-party means precompiled code authored by the Director or their coding agent and trusted at this layer, with defense in depth elsewhere. Dependency/artifact trust admission remains unresolved, not automatically inherited by linking. Native Rust callbacks can use ambient process authority; a type annotation does not sandbox them. A confined profile needs an observed isolation mechanism and has no independent external-effect authority. Denied confinement never falls back to native execution.

Any later script adapter must obey the finalized confined-computation boundary: bounded supplied inputs and private resources, no raw GPUI context, storage handle, file descriptor or unrestricted network client. The earlier proposal permitting guest operation requests is not an admitted egress route. Whether confined results may influence a later trusted output is an explicit information-flow proof obligation (G12), not solved by renaming egress as a return value. Script globals and GC heaps are not durable application state. No scripting engine is selected.

Code reload is a versioned transition: validate new code, reconcile persistent state through the host, invalidate affected caches, and retain or terminate old work under explicit policy. Incompatible state or missing imports produce a recoverable rejection; no silent reset of user work.

## Hosting remains an open engineering boundary

The trusted-native route is independently useful and does not require LOK/Nova/MOO services to exist on every user's machine. Constellation integration uses their published boundaries when selected.

The Director explicitly permits JS/Wasm hosting glue and hybrid HTML/GPU presentation, superseding the original strict-no-JS loader restriction. The proposed browser host chooses presentation automatically from semantic roles and capabilities, explains its choice and permits supported overrides. Useful HTML content and standard forms precede Wasm startup or remain usable without scripting. Visible native/HTML islands share focus, selection, navigation, overlays, accessibility and supported drag behavior with GPU regions. This is not yet a qualified integration or selected server framework.

GPUI remains the required default presentation foundation. Accelerated wgpu/WebGPU integration is a preferred candidate, not proof of universal fallback. When qualified GPU routes are unavailable, a declared compatibility experience preserves work and specifies unavailable GPU-specific functions; broad compatibility remains an obligation. A real incompatibility must be reproduced before proposing a different engine or fork. The framework does not make a mandatory server/deployment choice: developers choose hosting, and offload remains subject to data locality and authority.

Shared semantic destinations map to windows, browser history and mobile navigation. Automatic adaptation cannot omit ordinary phone UI workflows. Only genuine capability/execution limitations permit a declared developer-selected offload or skip. Switching renderer/layout preserves subject identity, drafts, history and admitted operations; input composition may defer a switch. Independent host tests must establish these outcomes rather than infer them from shared source.

Shared compute/render residency also remains unproved: GPUI using wgpu somewhere does not establish one shareable device, queue, allocator, or synchronization contract for our compute service. A renderer fork is not the default answer to that gap.

## Recovery and authority without routine friction

An opinionated replaceable standalone host supplies durable drafts and eligible local edit history without requiring LOK or making the renderer authoritative. Synchronization remains owned by the selected application/service, not a new Drishti sync engine. Framework-mediated removal preserves discoverable identity and recoverable content; normal work does not depend on an actor remembering backup commands. Retention, exhaustion and corruption behavior must be explicit before a durability claim. Recovery cannot undo external disclosure or intercept arbitrary trusted-native syscalls by assertion.

In constellation mode, consume LOK identity, scope, locality, grants and effect admission, and Braid content/vocabulary identities. The proposed identity/capability/intent/outcome equation is not already proved by the existing attenuation checks. Reconcile existing confirmation policies rather than silently bypassing them. Product policy is structural, low-friction operation semantics, not moral classification or a second Drishti permission authority.

## Inspectable intent and causal feedback

Compact scoped semantic projections let agents discover the same destinations/actions as humans without a separate app-specific sitemap or scripting layer. This is an agent interface, not a mandatory inference engine. Versioned incremental disclosure and resource accounting must demonstrate lower transfer and computation under matched tasks; they must not hide information required for a correct decision.

The framework provides source-linked causal records and headless pending/failed/denied/unknown/recovery contracts. Applications own error appearance within their design system. Diagnosis should take seconds on qualified cases, but unknown external causes remain unknown. Bounded redacted evidence must explain why work started, blocked or became obsolete without collecting all user data. Numeric diagnostic and egress targets remain calibration obligations.
