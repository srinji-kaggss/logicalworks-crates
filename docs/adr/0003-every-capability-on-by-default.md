# ADR 0003: every capability is on by default

Status: **accepted**, 2026-10-22. Director's order of 2026-10-22: "IF A FUCKING
THING EXISTS HERE, BY DEFAULT IT WILL BE FUKING ON … A DEVELOPER WILL FUCKING
COMPLY OR USE SOMETHING ELSE." Issue: #732. Supersedes `INV-DEP-2`'s "none is
default-on", `INV-DEP-6` and `WORKFLOW.md` §4's storefront carve-out.

Stakeholders: the Director; the executing agent; a consumer adding a crate to a new project; the std-first gate
Concerns: a capability that exists but cannot be found is rebuilt by hand; a flag makes discovery a prerequisite for use; compile time, binary size and target portability are the price
Requirements: R1, R6, R11, R13
Viewpoint: the capability viewpoint — one view per crate, a table of capabilities against owner module, cargo feature and default state
Elements and relations: the five crates, their modules, features and default sets; relations: `feature enables module`, `module owns capability`, `default set carries feature`

## Context

Every capability was behind a default-off feature: `hash`, `build`, `sign`,
`json`, `http`, `process` in `lgwks_std`; all 28 grammars, the `script!` reader
and the scanner in `lgwks_ast`; the drivers, inspection and identity in
`lgwks_bot`. The failure was measured on 2026-10-22 in Secure-Authority PR #149:
an agent needed a build-input digest, found no `lgwks_std` module because `hash`
was off and nothing named it, and wrote one with `sha2`. The std-first gate then
refused the copy and named an owner the agent could not discover. The gap was
the default, not the agent.

## Decision

1. **The default is the decision.** Each crate's `default` carries every
   capability it owns on every declared target: `lgwks_std` is `standard`
   (everything but `http`), `lgwks_ast` is `full, script, tool, scan` (all 28
   grammars, the reader, the tool, the detectors). `lgwks_bot` carries its
   portable boundary — `rt, time, sync, macros` — and `lgwks_deps` carries the
   empty selection; both are argued below and both are exceptions with a veto,
   not gaps.
2. **An opt-out states its number.** A capability is default-off only where a
   measurement or a target denies it, recorded here and in its catalog row.
3. **The opt-out is one flag.** `--no-default-features --features core` is the
   zero-dependency `lgwks_std`; `--no-default-features` is the derive-only
   `lgwks_ast` and the portable `lgwks_bot` core. A consumer says it once, at the
   edge, instead of assembling a surface from flags it must learn first.
4. **The target veto.** The default set is the set that builds on every declared
   target: `scripts/check-target-matrix.sh` builds `lgwks_std` and `lgwks_deps`
   with *no flags* as a required claim for `wasm32-wasip1`, `x86_64-linux-musl`,
   `x86_64-freebsd`, `aarch64-android` and `aarch64-ios`. A default that fails
   that claim is not a default, it is a broken build on a target we ship.

## The measurement

A consumer with no feature flags, this machine, warm registry, cold target
directory, 2026-10-22 (`cargo build --offline`, wall clock from `date +%s`;
packages from `cargo tree --offline --prefix none | sort -u | wc -l`):

| Consumer | Packages | Cold build | Result |
|---|---|---|---|
| old default (`core`, `trace`), using `trace` only | 27 | 6 s | builds |
| old default, using `trace`, `hash` and `build` | 27 | 5 s | **refused: seven capabilities absent** |
| new default (`standard`), using `trace`, `hash` and `build` | 107 | 34 s | builds and runs |

The consumer's `build.rs` digests through `lgwks_std::build::InputDigest` and
writes `rerun-if-changed` through `build::directive`; `main` hashes with
`lgwks_std::hash::Hasher` and installs `lgwks_std::trace::install_default`.
Portability the same day: `cargo check -p lgwks_std --target wasm32-wasip1`
exited 0 in 54 s. The workspace with the new defaults checked in 38 s warm.

The rows that are off, warm target directory:

| Row | Command | Cost | Why it is off |
|---|---|---|---|
| `lgwks_std/http` | `cargo check -p lgwks_std --target x86_64-unknown-linux-musl` | 7 s to fail | `ureq` → `rustls` → `ring` compiles C per target; the no-flag check above is required and failed with `cc-rs: failed to find tool "x86_64-linux-musl-gcc"` |
| `lgwks_deps` selections | `cargo check -p lgwks_deps --target wasm32-wasip1` | — | every group is native (`appcui`, `gpui`) or a driver with no wasm surface (`tokio-net`, `tokio-process`, `tokio-signal`) |
| `lgwks_deps/gpui` | `cargo check -p lgwks_deps --features gpui` | 122 s | `objc2` refuses to compile off Apple targets, so the default would break Linux and Windows |
| `lgwks_deps/ml-candle,ml-tokenizers` | `cargo check -p lgwks_deps --features ml-candle,ml-tokenizers` | 90 s | an inference runtime and a network-capable tokenizer closure, paid by every job that builds the crate |
| `lgwks_deps/ml-candle-metal` | inside `ml-candle` | — | the macOS Metal backend; it builds nowhere else |
| `lgwks_bot` host drivers | `cargo check -p lgwks_bot --target wasm32-wasip1` (the CI boundary step builds the defaults) | — | `net`, `process`, `fs`, `signal` and `inspect` have no wasm surface, and the T22 row asserts *on this default set* that no consumer can name a process description |

## Alternatives rejected

- **Keep default-off and publish a catalog instead.** #732's measured failure: a
  refusal that arrives after the work is not discovery.
- **Default-on for `lgwks_std` only.** Then `script!` and inspection stay hidden
  and the order is half executed.
- **Turn on `gpui` too.** It does not build on Linux or Windows; a default that
  is a compile error on a declared target is worse than an opt-out.
- **A new `all` feature instead of changing `default`.** A consumer would have to
  know to ask, which is the discovery problem again.
- **Move `http` into the default set and exempt its check.** That edits
  `scripts/check-target-matrix.sh`, which the Director's 2026-10-13 shell ban
  keeps out of this session's hands; the one-line change is named in #732.

## Consequences

- Cold builds grow: 6 s to 34 s, 27 packages to 107 for a `lgwks_std` consumer.
- Every gate job compiles the whole default surface; the lane table keeps one
  lane per feature set, so it is paid once per job. The `lgwks-bot full` suite
  job sits at its 300 s cap (263 s before this ADR) and needs a larger cap or a
  split; that is a `.github/workflows/ci.yml` change and is named in #732.
- `INV-DEP-2` and `INV-DEP-6` are replaced by `INV-DEFAULT-1`; the catalog
  (`contract/CATALOG.toml`, `docs/catalog.md`) is the one place a default is read.

## Correspondence

Supersedes the default-off doctrine of `INV-DEP-2`, `INV-DEP-6`, `AGENTS.md`
policy item 2 and `WORKFLOW.md` §4. Refines ADR 0004. Serves `R1`, `R6`, `R13`
and PRD 0001.

## Verification

`docs/catalog.md` prints every capability's default from
`contract/CATALOG.toml`; the `catalog` lane refuses a stale page, a missing
owner file and a feature its crate does not declare (`cargo run --locked -p
lgwks_deps --bin catalog -- --check`). The `docs-default` and `feat-*` lanes
build each crate's default set, and `target-matrix` checks `lgwks_std`'s default
set across the declared targets. The consumer journey above is a recorded run,
not a test in the tree.
