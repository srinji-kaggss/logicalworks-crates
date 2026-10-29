# Status history: dated amendments

## Requirements amendment — 2026-09-12

The current working-tree contract supersedes strict-no-JS hosting with permitted glue and hybrid presentation; records phone parity, structural recovery, layer-scoped first-party trust and LOK/Braid reuse; and captures all questionnaire decisions without labeling unresolved mechanisms as chosen. No runtime, dependency, package version, public release, commit or remote update is part of this amendment. Published 0.0.1 remains the earlier snapshot.

Amendment verification on macOS, 2026-09-12, using the pinned toolchain:

| Command | Observed result |
| --- | --- |
| `cargo fmt --all -- --check` | Passed after correcting one assertion's formatting |
| `cargo build --locked` | Finished documentation-crate dev build |
| `cargo clippy --all-targets --locked -- -D warnings` | Finished with no warnings |
| `cargo test --locked` | Six bundle tests passed; no runtime tests |
| `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --locked` | Generated Rustdoc |
| `cargo package --locked --allow-dirty` | Packaged 19 files and verified compilation; allow-dirty is required for this deliberately uncommitted amendment, not a bypass of compilation/tests |
| `cargo test --manifest-path target/package/drishti-0.0.1/Cargo.toml --locked` | Six extracted-package bundle tests passed |

Independent read-only review found four traceability gaps: a capability-negotiation reference pointed at removal recovery, the speculation journey omitted default admission/pressure shutdown, hybrid rendering omitted selection/inspection/override evidence, and the first-party trust source pointed at implementation research rather than Director authority. All were corrected; follow-up review closed all four. Review checked source consistency, not runtime behavior. G01–G14 remain runtime proof obligations; all 30 product journeys remain unmeasured. These local results do not claim a new hosted CI run or published amendment.

## Ink lab and GPU residency amendment — 2026-09-30

The Director instructed that drishti "feel like this ... as the gpu accelerated
runtime and the inklab", with drishti remaining the Rust framework (D15). Added
`src/design.rs`, `src/ink_lab.rs`, `src/runtime.rs`, `docs/inklab.md`,
`docs/gpu-runtime.md`, requirements R35–R37 and journeys J31–J33, and repaired a
pre-existing gate defect: the package's own `forbid` lints rejected
`tests/bundle.rs`'s `unwrap`/`expect`, so `cargo clippy --all-targets` failed on
the committed tree. The package allow-list now includes `src/*.rs`.

Verification on macOS aarch64 with rustc/cargo 1.98.0, 2026-09-30:

| Command | Observed result |
| --- | --- |
| `cargo fmt --all -- --check` | Passed after formatting the new modules |
| `cargo build --locked` | Finished the dev build with the realized seats |
| `cargo clippy --all-targets --locked -- -D warnings` | Finished with no warnings (library and tests) |
| `cargo test --locked` | 18 library tests and 6 bundle tests passed; the bundle tests validate the 37/33 registries |
| `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --locked` | Generated Rustdoc with the two new documents embedded |
| `cargo package --locked --allow-dirty` | Packaged and verified 25 files (`--allow-dirty` because the amendment is deliberately uncommitted) |
| `cargo test --manifest-path target/package/drishti-0.0.1/Cargo.toml --locked` | 18 library and 6 bundle tests passed from the extracted package |
| `cargo run --locked --example ink_preview` | Wrote `target/ink-preview.svg`: 48 sampled frames of the mark, 8,481 fused circles resolving to 360 crisp beam polygons (24 beams per resolved frame) |

These are library and deterministic-simulation results. They do not qualify a
GPU backend, a browser host, or any product journey; J13 and J31–J33 remain
unmeasured, and no real-device capture, transfer measurement, IME check or
accessibility observation was performed.

## Native GPU host amendment — 2026-10-06

The Director asked for "a fully working version of this based on our estate"
(D16). Added `src/native.rs` (feature `ui`), which presents the ink lab in a
GPUI window. GPUI comes from `lgwks_deps` 2.0.0's `gpui` storefront feature
(GPUI 0.2.2 in the lock file); there is no direct `gpui` or `wgpu` edge, and
the default build still has no dependencies. Also added `examples/ink_lab.rs`
and `RuntimeReport::uploads()`. While wiring the host, two runtime defects were
found and fixed, each with a regression test. First, a changed frame that
uploads zero bytes (a resolved, empty effect) was not delivered, so a backend
would keep painting the stale liquid frame. Second, turning reduced motion off
left resolved effects frozen.

This is how the code I have written or will write attempts to be frontier and
universal. The alternatives rejected were a direct `wgpu` renderer, which would
add a second presentation owner beside GPUI (D03), and a direct `gpui` edge,
which would bypass the storefront (D04). The host owns no frame logic. It
presents only what `Runtime::present` admits, holds one resident frame per host
per device epoch, and requests GPUI frames only while there is demand. The
supported domain is native macOS, where GPUI's Metal renderer runs. Windows and
Linux GPUI targets compile through the same feature but have not been run.

Verification on macOS aarch64 with rustc/cargo 1.98.0, 2026-10-06, one gate run
(9 s wall, warm):

| Command | Observed result |
| --- | --- |
| `cargo fmt --all -- --check` | Passed |
| `cargo clippy --all-targets --locked -- -D warnings` | Clean |
| `cargo clippy --all-targets --features ui --locked -- -D warnings` | Clean (cold GPUI build took 1 min 44 s) |
| `cargo nextest run --locked` | 29 passed |
| `cargo nextest run --locked --features ui` | 31 passed |
| `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --locked [--features ui]` | Both generated |
| `cargo package --locked --allow-dirty` | 28 files packaged and verified |
| `ink_lab --frames 600 --reduce-motion --json` | 1 presented frame, then idle and the app quit; 11 receipts, 576 bytes uploaded (24 crisp beams × 24 B), 0 paint failures, 0.12 s wall, 66.2 MB peak RSS |
| `ink_lab --frames 600 --reduce-motion --dark --seed 42 --json` | Same counts, 0.12 s wall, 65.5 MB peak RSS |
| `ink_lab --frames 600 --json` | **Not measured.** The screen was locked, so macOS delivered 2 frames and then none. The stall watchdog ended the run after 10.5 s with `NoFrames { presented: 2 }` (65.2 MB peak RSS) |
| `ink_lab --bogus`, `ink_lab --frames` | Usage error on stderr, exit 2 |

The full-motion journey has no frame-interval percentiles yet, because the
session that ran it was locked. It needs a rerun on an unlocked display. The
goo filter and text turbulence are not painted. J09, J13, J31–J33 remain
`unmeasured`: this is one native host on one machine, not an independent
journey oracle, an accessibility observation or a device capture.
