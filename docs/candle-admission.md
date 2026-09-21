---
type: Decision
title: Candle ML inference storefront admission
description: Candle ownership, default-off feature boundary, transitive-surface limits and verification plan.
resource: /docs/candle-admission.md
tags: [dependencies, candle, ml, inference, tokenizers]
decision: 2026-09-18
stale_after: 2026-10-18
sources: [/docs/dependency-doctrine.md, /contract/APPROVED.toml, https://github.com/huggingface/candle, https://crates.io/crates/candle-transformers/0.11.0, https://crates.io/crates/tokenizers/0.23.2]
---

# Candle ML inference storefront admission

This document records the approval of Candle as the ML inference runtime of the
`lgwks_deps` storefront, together with the alternatives measured against it, the
limits of its transitive surface, and the verification required before the
capability is relied upon.

## Decision

Candle is the selected inference runtime for embedding a System One-style
decision model in Rust applications within this workspace, with the direction
that the work be built here rather than in a separate project. The approval is
recorded with the date `2026-09-18`.

The approval record is the commit that adds the register blocks and the authored
edges, per `docs/dependency-doctrine.md` §7 and the register header. This
document is evidence of that approval; it is not a signed artifact.

## Ownership and alternatives

`std`, `lgwks_std`, `lgwks_bot`, and `lgwks_ast` supply no tensor compute,
neural-network layers, transformer model execution, or checkpoint-compatible
subword tokenisation. The alternatives considered were Burn 0.22.0-pre.3, the
`ort` ONNX Runtime bindings 2.0.0-rc.13, and `tch-rs`/libtorch. Candle was
selected because it provides a tested ModernBERT implementation, reads
safetensors directly, and reaches Metal on macOS; the tokenizer crate version
matches the reference Python runtime, which makes tokenizer parity
verifiable. Burn remains the later option and `ort` the
highest-fidelity fallback; neither is admitted by this decision.

`lgwks_deps` is the BOUNDARY rung owner with only `lgwks_deps` as a normal-kind
authored consumer. Candle reports MIT OR Apache-2.0; tokenizers reports
Apache-2.0.

## Transitive surface and egress

The register admits four authored edges only. Their transitive closure is
lockfile provenance, not package-level authority, and it is large. Selecting
`ml-candle` compiles `candle-transformers`, which depends on `hf-hub`; the
resulting tree is therefore network-capable. The intended runtime behaviour is
to load a local checkpoint without calling the hub; that intent is a runtime
property to be verified, not a property implied by this admission. No
advisory-clean or universal safety claim is made for the closure.

## Verification plan

1. The default and no-default-feature resolved graphs contain no Candle or
   tokenizers package; inspect `cargo tree` before and after feature selection.
2. The explicit `ml-candle` and `ml-tokenizers` features resolve and compile,
   and the re-exports `lgwks_deps::{candle_core, candle_nn,
   candle_transformers, tokenizers}` are reachable.
3. The real admission gate accepts the owned edges (`lgwks-deps check` prints
   OK) and the closure test continues to require every non-facade dependency to
   be `optional = true`, so the default build stays zero-dependency.
4. Workspace fmt, clippy, and tests remain mandatory, and the package smoke is
   unchanged.
5. CPU compilation is checked on the Linux CI job; the Metal backend is
   macOS-only and must be verified on a macOS job, not inferred.

## Assurance limits

This admission claims single ownership and opt-in compatibility and nothing
more. No frontier, hyperscale, portable-parity, or fastest-correct-Rust claim
follows from adopting a library. Fleet behaviour, physical Ryzen profiles, human
outcomes, production isolation, crash and recovery acceptance, and end-to-end
model parity remain unmeasured. Test processes are finite, artifacts are owned,
and disposal is Trash-only.

## Known duplication: CONSOLIDATE debt

`candle-transformers` 0.11 pins `tokenizers` 0.22 internally while this
admission authors `tokenizers` 0.23 for exact parity with the reference Python
runtime, so the `ml-candle` closure carries **two `tokenizers` versions**
(0.22.2 and 0.23.2). This is transitive, not an authored edge, but it is
duplication the CONSOLIDATE rung would normally retire. Resolutions, none yet
chosen: author 0.22 and lose exact Python parity; keep 0.23 and accept the
second copy; or drop `candle-transformers` and hand-write the encoder on
`candle-core`/`candle-nn` (also removes `hf-hub`). The decision is open.

## Local execution receipts

On the edited source, macOS arm64 (M-series), Rust 1.98.0:

- `lgwks-deps check .` — exit 0, **23 semantic approvals** (was 19), every
  authored external edge owned.
- `cargo metadata --locked` — exit 0.
- Default closure: `cargo tree -p lgwks_deps --no-default-features -e normal`
  contains **no** Candle or tokenizers package (facade only).
- `cargo check -p lgwks_deps --no-default-features --features
  ml-candle,ml-tokenizers --lib --locked`: exit 0; compiled `candle-core
  0.11.0`, `candle-nn`, `candle-transformers 0.11.0`, `tokenizers 0.23.2`; ML
  closure measured at **364 packages**.
- `cargo clippy --workspace --all-targets --locked -- -D warnings` — exit 0.
- `cargo clippy -p lgwks_deps --no-default-features --features
  ml-candle,ml-tokenizers --lib --locked -- -D warnings`: exit 0.
- `cargo fmt --all -- --check` — exit 0.
- `cargo test --workspace --all-targets --locked` — exit 0, 0 failures.
- `./scripts/lgwks-std-package-smoke.sh` — exit 0, "lgwks_std package smoke
  passed".

Not verified here: Linux/Windows compilation, the Metal backend
(`ml-candle-metal`), any actual model inference or numeric parity, and the
absence of `hf-hub` network calls at runtime.

