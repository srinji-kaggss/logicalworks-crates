# Status and evidence boundary

Version: 0.0.1. Date: 2026-09-12. Product stage: **design publication**.

## Present

- Original authored vision, architecture, structure, decisions, research synthesis, and assurance requirements.
- 37 traceable requirements and 33 specified human/agent acceptance journeys.
- The first realized presentation seats (Director instruction, 2026-09-30): Logical Works brand tokens (`src/design.rs`), the eleven-effect ink lab (`src/ink_lab.rs`), and the GPU residency model (`src/runtime.rs`). These are deterministic reference cores with unit tests; they emit frame geometry and receipts and do not claim a device.
- Finalized product contract with 40 initial answers, 14 follow-up answers, subsequent corrections, derived constraints and explicit remaining proof obligations.
- A dependency-free Cargo documentation package with Rustdoc modules and document-bundle integrity tests.
- Source-grounded blockers for browser hosting, GPUI integration, script selection/confinement, estate consumer support, resource calibration, and strict Keel admission.

## Absent / unverified

No accelerated backend (wgpu/WebGPU), UI, browser host, interpreter integration, sandbox, host authority adapter, shared compute/render residency, or performance result is implemented. The ink lab and residency model are deterministic reference cores, not a qualified renderer; `BytePerShape` transfers are modelled, not captured. All product journeys — including J31–J33 — remain `unmeasured`. Keel is not wired. No executable gate claims runtime acceptance.

The planned runtime tree is only partly realized: the on-disk seats are `src/design.rs`, `src/ink_lab.rs` and `src/runtime.rs`. Documentation modules remain a publishing mechanism, not placeholder runtime implementations. This package cannot be used to build a frontend today.

## Original publication verification record

Local verification on macOS aarch64 with rustc/cargo 1.98.0, 2026-09-12:

| Command | Observed result |
| --- | --- |
| `cargo fmt --all -- --check` | Passed after correcting reported formatting differences |
| `cargo build --locked` | Finished dev build of documentation crate |
| `cargo clippy --all-targets --locked -- -D warnings` | Finished without warnings |
| `cargo test --locked` | Five bundle tests passed; zero runtime unit tests and zero doctests |
| `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --locked` | Generated Rustdoc successfully |
| `cargo package --locked` | Packaged and verified the documentation crate; 18 files before Git provenance metadata |
| `cargo package --list --locked` | Inspected explicit archive allowlist: original docs, registries, package metadata, license, toolchain and bundle tests |

Initial verification caught a missing integration-test crate doc and a case-sensitive metadata assertion; these were corrected, then the failed/dependent checks rerun. Independent read-only document review caught a stale sibling GPUI-export observation and missing explicit typed-automation/denied-operation acceptance; the documents and registry now include those corrections. Structural checks cannot prove vision completeness; this review was distinct from the author but did not execute a product journey.

All checks must be rerun after final document changes and from the packaged source before release. Successful package checks do not change any product journey to passed. Hosted CI and publication have their own receipts; this pre-publication document does not claim either has succeeded.

Publication is separately verified through Cargo and the registry. An available name or successful dry run does not establish published ownership. GitHub may remain private while this original design documentation is publicly distributed through Cargo/Rustdoc, as explicitly authorized by the Director.

The dated amendments (requirements, ink lab and GPU residency, the native host) are in [status-history.md](status-history.md).
