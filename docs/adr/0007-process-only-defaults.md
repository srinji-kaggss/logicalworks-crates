# ADR 0007: process-only defaults hold without the substrate

Status: **accepted**, 2026-10-26. Issues: #687, #732. Amends ADR 0003 §1:
`lgwks_bot` default carries `ecs`, `trace`, `wire`, `ron` besides the
portable runtime; `--no-default-features` is the process-only opt-out.

Stakeholders: the Director; a consumer supervising processes; the catalog gate
Concerns: a process supervisor must not compile TLS, archive, UI, or subscriber
Requirements: R1, R6, R11, R13
Viewpoint: the capability viewpoint — one row per capability, its owner and default
Elements and relations: `lgwks_bot` features and their modules; `default set carries feature`

## Context

`lgwks_bot` depended on `lgwks_std` with all defaults and reached `bevy_ecs`
unconditionally through `lgwks_deps`. A `default-features = false` consumer
compiled 249 dependency lines including `ring`, `rustls`, `ureq`, `bevy_ecs`,
`rkyv`, `ron`, and `tracing-subscriber`, with a 51 s cold build carrying a C
tool-chain. The failure is #687; the default rule is #732/ADR 0003.

## Decision

1. `lgwks_bot` depends on `lgwks_std` with `default-features = false` and
   exactly `json`, `hash`, `trace`. `net` adds `http`, `process` adds
   `process`, `ephemeral` adds `random`, `wire` and `ron` carry the encodings.
2. Default is `script`, `ecs`, `trace`, `wire`, `ron`: every estate capability
   on by default on every declared target (ADR 0003). The ECS substrate is how
   a bot executes; the facade, wire, and RON are what it observes and persists.
3. `--no-default-features` opts a process-only consumer out of all five.
   `process` implies `io`, `sync`, `time` (hence `rt`) and never `ecs`,
   `trace-install`, `wire`, `ron`, `script`, or `inspect`.
4. Erased registry handles (`Source`, `Action`, `Condition`, `DomainRegistry`)
   and the `domains!` surface ride `ecs`/`script`/`inspect`; the file journal
   rides `wire`; HTTP rides `net`. No public path moves.

## Alternatives rejected

Edge-exactness without the substrate gate: remove `bevy_ecs` from the one
`lgwks_deps` edge but leave every module ungated. Rejected: the seven crates
leave the tree, but `domain::net`, `idempotent`, and the erased registry still
compile against absent types, so the next consumer meets the same refusal one
module further down. Cost was another pass over the same 40 files.

Docs-only acknowledgement: record the closure and concede the tree.
Rejected: SD-11 forbids narrowing the vision to make a claim easier, and #687
names the matched proof (`cargo tree` with none of the seven), not a note.

## Consequences

A process-only consumer pays 32 packages and a 19.5 s cold build with no C
tool-chain; a default consumer keeps the full substrate, encodings, and
installer. The `debug-doctor` feature on `lgwks_deps` carries the doctor's own
subscriber so the storefront library never does by unification. Open: the 141
`dead_code` warnings under `--no-default-features` (wire-gated file journal,
`ecs`-gated broker/difference) are warnings, not errors; silencing them by
gating the file module behind `wire` is a later decision with its own evidence.

## Correspondence

Amends ADR 0003 §1 (the default set) and refines ADR 0006 (one ECS world: the
world is default-on, and the opt-out is one flag). Related: #732 (catalog owns
every capability), #526 (the supported surface is `script!`; ceilings in
`contract/PUBLIC_SURFACE.toml` move with the vocabulary).

## Verification

`catalog` lane (`cargo run --locked -p lgwks_deps --bin catalog -- --check .`);
`storefront_consumers::proc_only_consumer_carries_no_deselected_crate`
(`cargo tree` holds none of the seven);
`cargo check -p lgwks_bot --locked` (exit 0, no warnings) and
`--no-default-features` (exit 0);
`cargo test -p lgwks_bot --locked --features full` runs every gated path so
none rots. `docs/catalog.md` is a fresh render of the register.

## Measurement

Process-only fixture
`crates/lgwks-deps/tests/fixtures/process-only` (`lgwks_bot`,
`default-features = false`, `features = ["process"]`), this machine:

| Closure | Lines | Packages | Cold build | Peak RSS | Result |
|---|---|---|---|---|---|
| before (seven crates) | 249 | 134 | 51.07 s | — | builds with TLS/archive/ECS/subscriber |
| after (none of seven) | 46 | 32 | 19.52 s | 602 MiB | `cargo tree` holds no `bevy_ecs`, `rkyv`, `ron`, `tracing-subscriber`, `ureq`, `rustls`, `ring` |

Default (`cargo check -p lgwks_bot --locked`) stays exit 0 with no warnings;
`--no-default-features` stays exit 0; `full` carries every feature so none rots.
The `catalog` lane and `storefront_consumers::proc_only_consumer_carries_no_deselected_crate`
hold the closure; `no_default` holds the sync surface under `ecs` without `rt`.
