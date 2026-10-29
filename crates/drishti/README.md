# Drishti

**Frontend is orchestration. Creators express experiences; the framework owns the coordination that makes them coherent.**

**Version 0.0.1 published the documentation-only design package; 0.1.0 publishes the first realized presentation layer:** the Logical Works ink lab (`src/ink_lab.rs`), its brand tokens (`src/design.rs`), the GPU residency model (`src/runtime.rs`), and a native GPU host (`src/native.rs`, feature `ui`) that presents ink frames in a GPUI window. GPUI arrives through the estate storefront (`lgwks_deps/gpui`). A browser host, the goo filter (GPUI 0.2 has no blur or threshold pass), and a qualified Keel integration are still absent.

```text
cargo run --release --features ui --example ink_lab                       # open the ink lab
cargo run --release --features ui --example ink_lab -- --frames 600 --json # measured session
```

Drishti is a Rust-first frontend framework design grounded in immersive, context-preserving interaction: documents recall where work stopped, workflows become inspectable routes, and people compose automation from meaningful operations rather than callback plumbing. Ordinary forms and conventional applications remain first-class; immersion is not compulsory animation or a mandatory 3D world.

The default presentation language is the Director's Logical Works brand — **flat build, liquid motion**: semantic tokens that name roles, nine flat colourways, and eleven ink effects that always settle on the crisp static mark. The brand is not a skin over the framework; its "everything resolves" rule is the same requirement as reduced-motion equivalence and accessible navigation, so the ink lab is part of the correctness surface.

The vision is primary. Universality, ergonomics, security, efficiency, accessibility, portability, and evidence are constraints and comparison axes—not competing product identities. Merely replacing JavaScript syntax, using a GPU, or adding another graph is not the value proposition.

## Read in order

1. `VISION.md`: product identity, authoring promise, universality, and reference experience.
2. `docs/architecture.md`: ownership, orchestration, effects, presentation, and limits.
3. `docs/codebase.md`: complete planned module structure and responsibility boundaries.
4. `docs/decisions.md`: finalized product contract, all 40 answers and conflict-resolution provenance, source conflicts, and blocking proof obligations.
5. `docs/research.md`: scripting, prior art, and what evidence changed the design.
6. `docs/assurance.md`: strict acceptance and eventual Keel integration.
7. `docs/inklab.md`: the Logical Works brand as the presentation layer — tokens, the eleven ink effects, and the adaptive engine.
8. `docs/gpu-runtime.md`: the GPU residency model — demand scheduling, budgets, reuse, transfer accounting, and device epochs.
9. `contract/requirements.tsv` and `experience/journeys.tsv`: normative requirements and falsification cases.
10. `docs/status.md`: what exists, what was checked, and what remains unproved.

The same narrative documents are embedded in the crate's Rustdoc modules so they remain readable without access to the GitHub repository. The TSV contracts are also embedded in Rustdoc and included in the Cargo package.

## Foundation decisions

- **GPUI** is the selected presentation foundation, not code to rebuild because of a GPL misconception. Its framework license is Apache-2.0.
- **logicalworks-crates first:** `std`, then `lgwks_std`, `lgwks_bot`, and optional third-party features owned by `lgwks_deps`; `lgwks_ast` remains the estate parser surface.
- **One authoritative application-state owner:** supplied by the application host; in the Logicalworks constellation, LOK owns durable state and effect authority. Drishti owns derived presentation and bounded execution bookkeeping, not a competing OS.
- **No mandatory AI, browser, scripting VM, or per-pane sandbox** in the trusted-native baseline. Isolation remains mandatory for any profile claiming to run hostile code.
- **Rust-first authoring; JS/Wasm glue and hybrid HTML/GPU presentation are permitted.** Useful HTML/forms before Wasm, coherent visible islands and broad desktop/mobile compatibility are requirements, not implemented capabilities. This supersedes the initial strict-no-JS restriction. Native proof cannot stand in for browser proof.
- **Experience-as-Code contributes contract and evidence discipline**, not an adopted runtime or a claim that its current browser provider is suitable.
- **The Logical Works brand is the default presentation language.** Its tokens and ink effects are first-party reference code; a GPU backend is admitted through `lgwks_deps`, never imported directly. The brand's "everything resolves to the crisp state" rule is enforced as reduced-motion equivalence, not decoration.

## This package

Apache-2.0, a member of the logicalworks-crates workspace since ADR 0006 R6, which merged this repository with its history. The library depends on `lgwks_std` for its hex encoding and trace events. The default `cli` feature builds the `drishti` command, which orchestrates through `lgwks_bot`'s `script!`. The optional `ui` feature adds `lgwks_deps` with its `gpui` storefront feature. There is no build script, telemetry or network client. This is an active project's design publication plus its first presentation seats, not a usable UI framework yet.

```text
drishti tokens                      the brand: primitives, both themes' roles with WCAG grades, colourways, CSS
drishti ink --seed 7 --frames 90    every effect on its own seeded engine: replay digest, shape bounds, label
drishti present --frames 120        a residency session through drain, loss and recovery, timed per frame
drishti limits                      every bound the engine and the runtime declare
drishti docs [PAGE]                 the design publication, one page at a time
drishti lab                         the native window (feature `ui`)
```

Each prints one JSON document; a refusal is one JSON line on stderr with a failing status. `tests/cli.rs` drives the built binary.

Verification, from the workspace root:

```text
cargo clippy -p drishti --all-targets --locked -- -D warnings
cargo clippy -p drishti --all-targets --features ui --locked -- -D warnings
cargo nextest run -p drishti --locked
```

These commands do not discharge runtime, security, experience, performance, or Keel obligations; G01–G15 in `docs/decisions.md` stay blocked. There is deliberately no empty runtime scaffold or green placeholder lane.

## Finalized requirements amendment

The 2026-09-12 contract amendment in `docs/contract.md` captures the Director's answers and subsequent corrections, including phone workflow parity, structural recovery instead of prompt friction, first-party trust at the chosen layer, and reuse of LOK/Braid authority contracts. Engineering derivations and open proof obligations are labeled separately. The working-tree amendment is not a republication of the immutable 0.0.1 release and does not authorize runtime implementation.

## Publisher

Logical Works Incorporated publishes `drishti` as one of six independently versioned crates in [logicalworks-crates](https://github.com/srinji-kaggss/logicalworks-crates#readme): `lgwks_std`, `lgwks_bot`, `lgwks_ast`, `lgwks_macros`, `lgwks_deps` and `drishti`.
[Should I use logicalworks-crates?](https://github.com/srinji-kaggss/logicalworks-crates/blob/HEAD/docs/should-i-use.md) says when to reach for each one and when to skip it.
