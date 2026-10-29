# Research basis and limits

Research date: 2026-09-12. This is source/document research, not a comparative benchmark or runtime audit. Author-reported results remain reports. No performance, isolation, accessibility, or portability claim is promoted to verified by this package.

## What changed the design

The Director's goal is not a new GPU API. It is to offload coordination nuance into a deterministic framework while keeping creative and domain decisions with the author. Logic OS provides a concrete frontend style; the browser repository provides semantic interaction principles; both contain older proposals that must be distinguished from current authority.

The current Logic OS charter and ADR-101 supersede older state-ownership and host requirements. Game-dynamics research supplies context recall, visible workflow routes, typed automation, and demand-shaped working sets. ADR-099 explicitly leaves focus-distance/LOD questions open. The new framework inherits the experience goals, not the claim that those mechanisms are solved.

The browser's compatibility promise is to preserve application semantics while removing runtime/quirk obligations. Its documents disagree about owned vs driven renderers and agent-first vs human-browser identity. Those are not silently resolved by selecting whichever paragraph supports a preferred implementation. Drishti is a human-facing frontend framework with an agent-accessible semantic surface; it is not a replacement browser project.

## Competitor mechanisms, not marketing rankings

| System | Existing mechanism | Consequence for Drishti |
| --- | --- | --- |
| Tauri 2 | System WebView, IPC, capabilities/scopes for frontend-to-core access | Useful authority-boundary precedent; not a DOM replacement or a sandbox for arbitrary Rust core code |
| Dioxus 0.7.10 | Signals, reactive resources, cancellation/restart on dependency changes, component task lifetime | Automatic invalidation/cancellation is prior art, not sufficient differentiation |
| Dioxus Native / Blitz | Native HTML/CSS renderer with its own DOM abstraction; README says beta | Removing the WebView need not remove DOM-oriented semantics |
| SwiftUI | Observed state, identity, bindings, view-associated tasks, standard-control accessibility | Learn semantic leverage: one declaration establishes several coherent behaviors; do not copy only syntax |
| React | Interruptible transitions; higher-level action/form abstractions handle common ordering | Compare modern competent usage. Custom transition workflows still have explicitly documented ordering obligations |
| GPUI | Entities, layout/prepaint/paint, text/input, platform integration | Selected reusable presentation foundation; not an already-proved complete orchestration contract |

Tauri's own capabilities documentation states that malicious/insecure Rust, intentional Rust bypasses, and incorrect command scope checks are outside that boundary. Swift task cancellation requires the running code to cooperate. Dioxus cancels a local future; that cannot by itself undo a remote side effect. These distinctions shape operation lifecycle and authority requirements.

GPUI upstream has Apache-2.0 `gpui`, `gpui_web`, and `gpui_wgpu` manifests. Its web path uses `wasm-bindgen`, `js-sys`, and DOM host types. The Zed editor's GPL license does not make GPUI GPL, but copying unrelated editor code would require its own license review. A source manifest is not a tested or published target profile.

## How embedded scripting works

An embedding normally supplies source -> parsing/compilation -> an executable representation -> an evaluator/VM with values and scope -> registered host calls. The language controls its own computation. The host controls which external operations are reachable and under which authority. Syntax convenience, execution engine, host API, and confinement are four separate decisions.

### Rhai

Rhai is a Rust-embedded scripting/evaluation engine. `Engine::compile` produces a reusable AST; `eval_ast` evaluates it against the selected scope and registered functions. Compiling once avoids reparsing unchanged programs. Dynamic values and Rust host functions make it suitable for a control layer without introducing a new standalone language toolchain.

Rhai provides expression/call depth, operations, variables, imports, and collection/string limits. Its operations limit is unlimited by default (`0`); `unchecked` disables safeguards. Crucially, one external Rust call may count as one operation while consuming arbitrary time or memory. Interpreter fuel therefore does not bound host calls. Module resolution, mutable host objects, and native registrations belong in the threat model.

A Rhai AST is not a portable Drishti wire format or an automatically inferred complete effect/dependency graph. Pin language/engine semantics; do not expose internals or mutable GPUI contexts to scripts. The library's sandbox claim is about accessible environment, not proof of OS process isolation.

### Lua

Lua compiles chunks into instructions executed by a VM. Its embedding C API uses a virtual value stack; this API stack should not be confused with the VM's register-oriented instruction operands. Tables, closures, userdata, garbage collection, and coroutines support a compact extensible control language.

Lua 5.4 free names resolve through `_ENV`; restricting that environment and loaded libraries controls reachable facilities. Coroutines are cooperative execution, not automatic CPU parallelism or transaction boundaries. Protected calls report errors; custom allocation and instruction hooks support limits. Instruction-count hooks run during Lua instructions, not arbitrary time spent in C host calls. Unordered table traversal, external time/randomness, host callbacks, and version-specific numeric/bytecode behavior complicate replay.

The Lua 5.4.8 manual explicitly warns that malicious bytecode can crash the interpreter because internal bytecode consistency is not checked. Do not accept arbitrary guest bytecode; a guest profile must choose source/trusted compilation and restrict native loading, debug, IO, package, and OS access as appropriate. This is a researched 5.4 profile, not a statement that 5.4 is the newest Lua release.

### Luau and mlua

Luau is a distinct Lua-derived language/runtime, not an interchangeable Lua version. It removes unsafe library facilities, supports readonly shared builtins and per-script environments, and provides interrupts. Its documentation still warns about native host functions, unbounded default memory, bytecode trust, and the value of separating trusted/untrusted VMs. Static typing does not replace authority checks.

`mlua` provides Rust bindings for Lua-family engines, including async bridging and Luau-specific sandbox/interrupt facilities. A safe Rust wrapper around an interpreter is not proof that every registered host function is safe, bounded, or authority-correct. No `mlua`, Lua, Luau, or Rhai dependency is admitted by this package.

### Decision implication

Do not design a new language or choose an interpreter to make a diagram complete. Rust declarations are primary. A script adapter earns admission if it materially improves customization, author iteration, or user-composable automation while lowering into the same semantic contract. Compare the cost of parsing/compilation, warm execution, host crossings, GC/allocation, reload/migration, errors, and confinement on real tasks.

The initial proposal allowed a script to propose a derived description or operation request. The later confined-computation direction supersedes that as an admitted effect interface: no independent direct/indirect egress authority, and the computed-result information-flow contract remains unresolved. Optional scripting must not recreate JS's hidden callback coordination under another spelling.

## Speculative tool execution: the transferable result

LLMCompiler moves known independent work off a serial reasoning loop. PASTE and SPORK predict future tool calls and overlap them with generation, reconciling candidates with actual requests. Speculative Programmatic Tool Calling recognizes work in partial programs and evaluates safe dependencies in a shadow environment. These are related but not identical techniques.

The transferable rule is to separate specification order, safe execution order, and authoritative adoption. Only legal work is reordered. The deterministic runtime owns dependencies, budgets, provenance, cancellation, and adoption. A predictor supplies candidates; it never becomes the authority.

Psi's source gives a concrete example: a read-only allowlist, revision-bound exact cache keys, adoption of a result/future, and invalidation. That does not prove external file changes are always tracked or cancellation kills every underlying effect. Another toolspec repository reports a synthetic simulation; its 'miss is never slower' statement does not establish that property under real contention.

PASTE reports 43.5% lower average completion time in its workloads; SPORK reports an 18% GAIA P95 reduction for Qwen3-32B; sPTC reports generally 1–1.2x on its RLM experiments. These are source claims, not reproduced results, and are not Drishti performance targets. Correctness and total cost must be evaluated independently of a headline speedup.

## Experience-as-Code and Keel

Experience-as-Code distinguishes declaration, validation, plan, observation, evaluation, receipt, state, and drift. That is useful design discipline. Its current process-journey schema is not a ready-made native-GPUI journey schema. Source inspection also found a simulated browser provider wired to the core; do not reuse its `Passed` status as live-browser evidence.

Keel's current local checkout lacks a complete executable/consumer contract. Historical strict standards are useful requirements inputs, not usable commands or current green evidence. Details and source identity are in the assurance document. No detector implementation or proprietary Keel source is redistributed here.

## Source register

Paths below identify research inputs, not dependencies of this package. Local checkout HEADs are context, not proof that every working-tree file matched HEAD. Referenced private repositories may require access. Narrative findings are newly authored; no sibling source tree is bundled.

| ID | Source and inspected scope |
| --- | --- |
| S01 | Director's instructions in this session, 2026-09-12: frontend=orchestration; vision primary; Rust/no-JS; GPUI; estate-first; Drishti; design publication |
| S02 | `logic-os-kernel` checkout HEAD `7fe9ab14dbb6a9f654fe9da88dbfe43371247fca`: `VISION.md`, `REPO_MAP.md`, `laws/CONSTELLATION-CHARTER.md`, ADR-099/101, `docs/canvas-architecture.md`, `docs/research/game-dynamics-runtime-physics.md` |
| S03 | `next-gen-browser-engine` checkout HEAD `50a7127ca6f56f15574681b3df33b9ae731b06f1`: `docs/COMPATIBILITY_CONTRACT.md`, `MACHINE_HUMAN_MEETING.md`, `AXIOMS.md`, `ADR.md`, `HUMAN_LENS_DEFERRAL.md`, architecture execution/vision docs; conflicting status retained |
| S04 | `logicalworks-crates` checkout HEAD `c8f4b8145cd9c4080795a43fc175b9db7f7c4148`: `AGENTS.md`, `docs/dependency-doctrine.md`, `crates/lgwks-deps/Cargo.toml`, public library source |
| S05 | `experience-as-code` checkout HEAD `90880fe8077a56fad52f903d3b73b5e264fe60a7`: `docs/TRUTH_MODEL.md`, experience schema, provider/core source, `scripts/check.sh`, `scripts/keel-floor.sh` |
| S06 | GPUI upstream HEAD observed `9d272b036335401f339d024ea94968fd51016c40`; live source retrieval: [GPUI manifest](https://github.com/zed-industries/zed/blob/main/crates/gpui/Cargo.toml), [web manifest](https://github.com/zed-industries/zed/blob/main/crates/gpui_web/Cargo.toml), [platform](https://github.com/zed-industries/zed/blob/main/crates/gpui_platform/src/gpui_platform.rs), [wgpu manifest](https://github.com/zed-industries/zed/blob/main/crates/gpui_wgpu/Cargo.toml) |
| S07 | [Tauri capabilities](https://v2.tauri.app/security/capabilities/), [security](https://v2.tauri.app/security/); read official tauri-docs v2 source |
| S08 | [Dioxus resource source v0.7.10](https://github.com/DioxusLabs/dioxus/blob/v0.7.10/packages/hooks/src/use_resource.rs), [Blitz README](https://github.com/DioxusLabs/blitz) |
| S09 | Apple documentation via local sourcewell corpus and Context7: [State](https://developer.apple.com/documentation/swiftui/state), [Task](https://developer.apple.com/documentation/swift/task), [accessibility](https://developer.apple.com/documentation/swiftui/accessibility-fundamentals), SwiftUI `task(id:)` and `id(_:)` |
| S10 | [React useTransition](https://react.dev/reference/react/useTransition), including its out-of-order caveat and higher-level action alternatives |
| S11 | Rhai book: [AST](https://rhai.rs/book/engine/compile.html), [sandbox](https://rhai.rs/book/safety/sandbox.html), [safety](https://rhai.rs/book/safety/index.html), [operation limits](https://rhai.rs/book/safety/max-operations.html) |
| S12 | [Lua 5.4.8 manual source](https://github.com/lua/lua/blob/v5.4.8/manual/manual.of), especially environment, C API, hooks, binary chunks; Lua VM source surfaced by Context7 separately versioned |
| S13 | [Luau sandbox](https://luau.org/sandbox), [mlua source/docs](https://github.com/mlua-rs/mlua), Context7 official-source discovery |
| S14 | [LLMCompiler](https://github.com/SqueezeAILab/LLMCompiler), [PASTE v3](https://arxiv.org/abs/2603.18897v3), [SPORK v1](https://arxiv.org/abs/2607.03333v1) |
| S15 | [sPTC](https://alexzhang13.github.io/blog/2026/spec-ptc/), [Psi speculation source](https://github.com/Subjective/psi/blob/main/psi-core/src/speculation.rs), [toolspec simulation](https://github.com/joelvarun/toolspec) |
| S16 | Keel historical baseline `37dd4667a4724128d1961defd43909cefbfe774b`, source references in assurance; current checkout incomplete |
| S17 | [crates.io policy source](https://github.com/rust-lang/crates.io/blob/main/svelte/src/routes/policies/+page.svelte): name squatting, genuine purpose/development, first-publication ownership; registry availability check is not reservation |
| S18 | Global frontier-universal assurance contract, read 2026-09-12: research gate, DMAIC/DMADV, traceability, independent evidence, bounded resources, Director counterexamples |
| S19 | Director's 40 questionnaire answers, B1/B1B answers and subsequent corrections in this conversation, 2026-09-12; normalized provenance in `docs/decisions.md`; explicit JS/Wasm glue approval supersedes S01's strict-no-JS clause |
| S20 | Director-supplied `reactive_imperative_borrow_checking_deep_dive.md`, 612 lines read from Downloads: hybrid mutation/observation, arenas/actors, static versus dynamic checks and semantic verification hypotheses; not reproduced or accepted as technical proof |
| S21 | Local LOK `laws/governance/adr-062-intent-outcome-gating.md` (PROPOSED), `kernel/crates/canvas-syscall/src/authorize.rs`, `kernel/crates/canvas-backend/src/effect_gate.rs`; Braid `crates/braid-capability/src/lib.rs` and verifier attenuation search in `crates/braid-verify/src/lib.rs`; read/search evidence, not executed integration tests or a new commit-identity claim |
| S22 | Director-supplied Logical Works brand package (`LGWKS-BRAND`), read 2026-09-30: `DECISIONS.md`, the design system's foundations and Ink lab, `system/lw-tokens.json`, `system/lw-ink.js` (the eleven-effect liquid engine), `system/lw-system.css`, `system/lw-motion.css`, `system/lw-dock.js`, the brand guidelines' motion rules and the `mxmail-web` example host. The JavaScript engine is the behavioural source for `src/ink_lab.rs`; the token file is the source for `src/design.rs`. Spec reading only, not a runtime audit of the reference implementation |

## Follow-up research disposition

The supplied state/borrow-checking brainstorm is a design input, not an adopted compiler architecture. Its hybrid imperative transition/reactive observation model motivates coherent snapshots and explicit async boundaries. Claims of zero-cost static dependency tracking, general restrictions on cyclic Rust graphs, exhaustive typestate expansion, and universal automated pointer proofs require independent primary-source verification. Do not inherit those claims, copy its illustrative unsafe code, or conclude that Drishti needs a new borrow checker. Compare owned arenas, message passing and proof-backed techniques only against concrete semantics and measured costs.

LOK ADR-062 proposes `Authorization = F(Identity, Capability, Intent, Outcome)` plus a semantic reason. Its mathematical-prevention wording is a proposal claim, not proof. The inspected authorizer represents app identity, typed scope, locality and standing grants, with separate mutation/egress axes. Braid's verifier search exposes attenuation checks; its capability token is not LOK's authority envelope. Neither observation proves full intent/outcome enforcement or an end-to-end sandbox. LOK's separate effect gate includes confirmation-oriented policy, so the frictionless product goal requires explicit reconciliation rather than assuming every existing layer already implements it.

Later product choices permit hosting glue, automatic hybrid presentation and visible islands. A wgpu/WebGPU candidate does not establish HTML/form behavior, unified input/accessibility, or a backend on every device. Wide compatibility, physical capability restrictions and real cross-host workflows remain acceptance obligations.

## Retrieval and proof limits

Local `lgwks-research --help` failed with `ModuleNotFoundError: No module named 'lgwks_research'`; the attempted 9Router search endpoint returned a 404 page. Primary GitHub sources, official documentation, arXiv pages, Context7, and the local Apple corpus were used instead. The shared corpus query returned no text and supplies no evidence for a claim. No live BrowserOS/native UI session was exercised.

`cargo search drishti` returned related names but no exact crate; `cargo info drishti` reported the exact package absent. A separate direct HTTP request returned 403. Only successful publication plus registry confirmation can establish the published name/version; search absence cannot.
