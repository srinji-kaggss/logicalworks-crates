# std × ast × deps closure matrix

Status: **an index of what exists and what each claim rests on, at one exact
revision.** It is not a certificate, and completing a row here does not close
its owning issue. Tracker: [#170](https://github.com/srinji-kaggss/logicalworks-crates/issues/170),
sibling trackers [#155](https://github.com/srinji-kaggss/logicalworks-crates/issues/155)
and [#156](https://github.com/srinji-kaggss/logicalworks-crates/issues/156).

**The revision this page describes is `d55ee4e22` (`origin/main`, 2026-10-11),
and every row's `test` column names a test that exists in that tree.** It was
first written against `b4a9f074` and re-pinned at `977c79d4` on 2026-10-05
([#280](https://github.com/srinji-kaggss/logicalworks-crates/issues/280)). On
2026-10-11 ([#416](https://github.com/srinji-kaggss/logicalworks-crates/issues/416)),
463 commits later, every test name Part 1 cites (40) was found again by
`codegraph query <name>`, every named test file and seeded family (28) was found
as a file, and the reconciliation added the one module that had landed without a
row (`sign`, #316). The revision is named once, here, rather than repeated per
row, because a per-row revision is a per-row value that rots exactly like the
prose version pins this matrix exists to replace: the honest form is *this page
describes one tree, and the tree is named here*.

The last column records the issue that **closed** a row's open item and its
date, plus any seeded family the closing work added; a row still open names its
open owner.

## What a row means, and what it does not

Every row names a **real test in this tree**. That is the whole claim, and it is
a weaker claim than it looks, so the weaker claim is stated rather than left to
be inferred:

| State | Meaning | What it does **not** mean |
|---|---|---|
| `exercised` | A named test in this tree drives the public surface and observes the stated behaviour on this exact revision. | That the test is an *external* observation, or that the row's remaining open work is closed. |
| `present` | The module/symbol exists and compiles under its declared feature; a test exists but does not cover the stated property. | That the property holds. |
| `unexercised-gap` | A named defect or ergonomic gap is open against this row, with an owning issue. | That the module is broken. |
| `assurance-gap` | The code makes a claim (constant-time, supply-chain, advisory-lock scope) that source inspection cannot discharge. | That the claim is false. It is unproven, not disproven. |

**These distinctions are load-bearing and are not collapsed anywhere below.**

- A passing unit suite is **not** an external-effect observation.
- A compile-time refusal (a `t22_process_surface.rs`-style consumer probe) is
  **not** an executed sanctioned run.
- `cargo check` of a feature combination is a **build** receipt, not a
  behavioural one.
- An admitted dependency edge in `contract/APPROVED.toml` is a **policy
  decision**, not an implemented capability.
- `kernel-check` of `proofs/bot-spec.sml` is an abstract-model proof, **not** a
  refinement proof of this Rust.

## Part 1 — the twenty-two `lgwks_std` modules

One row per `pub mod` in `crates/lgwks-std/src/lib.rs`, on its own page,
[Part 1](std-ast-deps-closure-matrix-std.md), checked against `lib.rs` by
the `check-doc-citations` binary of `lgwks_deps`.

## Part 2 — `lgwks_ast`: finished, and explicitly not re-opened

`lgwks_ast` is the finished standalone parser. It is complete rather than
parked, so it is not a candidate for new capability, and this matrix does not
propose growing it into a fourth surface.

| Property | State | Evidence on the pinned revision |
|---|---|---|
| 28 declared grammars, default 7 | exercised | `DECLARED_GRAMMARS: usize = 28` (`crates/lgwks-ast/src/parser.rs:3169`) with a fixture table asserted to cover every declared grammar; `default =` in `Cargo.toml` lists 7 `lang-*` |
| Every declared grammar accepts its valid fixture and refuses its malformed one without a recovery node | exercised | The `GrammarFixture` table and its completeness assertion (`crates/lgwks-ast/src/parser.rs:3321-3326`) |
| Depth-bounded traversal, typed diagnostics with spans | exercised | Retained repair; see the crate's own tests |
| **Not claimed here** | — | In-process bot code inspection is [#150](https://github.com/srinji-kaggss/logicalworks-crates/issues/150)'s, and parser/detection/inspection semantics are [#165](https://github.com/srinji-kaggss/logicalworks-crates/issues/165)'s. **Neither is verified by this matrix.** |

## Part 3 — `lgwks_deps`: admission is a decision, not a capability

| Property | State | Evidence |
|---|---|---|
| Every authored external edge has an owner | enforced by a gate | `cargo run -p lgwks_deps -- check .` (gate lane `deps-register`); `contract/APPROVED.toml` |
| `lgwks_std` and `lgwks_ast` author edges directly under the grandfather clause | enforced | `contract/APPROVED.toml`; the `check-std-first` binary of `lgwks_deps` reads the *source*, not the manifest |
| Feature-gated storefront capabilities are default-off | present | `crates/lgwks-deps/Cargo.toml` `[features]` |
| **`appcui` / `gpui` / `candle-*` / `tokenizers` / `bevy_app` / `bevy_time` / `bevy_state` are admitted dependencies** | **admitted-not-implemented**, marked by a gate | An approved edge is a recorded policy decision, **not a CUA product, a model runtime, or a delivered capability.** Each record's `reason` opens with `policy-only:`; the `check-std-first` binary of `lgwks_deps` (lane `std-first`) refuses an unconsumed storefront edge left unmarked and a consumed one still marked (#418). `bevy_ecs` is consumed (`lgwks_bot::ecs`) and is not marked. |
| Authored-edge assurance vs resolved transitive bytes, patches, source replacement, vendor integrity | **assurance-gap** | A no-deps graph is not a supply-chain proof. Transitive bytes: cargo checks each `.crate` against `Cargo.lock`'s sha256 under `--locked`. Patches, `[replace]` and source replacement are refused by `lgwks-deps check` since #517, and the vendored tree is gone (#519, option B), so [#266](https://github.com/srinji-kaggss/logicalworks-crates/issues/266) closed: INV-DEP-17 names the tests, which landed after this page's revision and so are not cited as rows here |
| Unambiguous register parsing, complete member records, collector error-path cleanup | unexercised-gap | [#157](https://github.com/srinji-kaggss/logicalworks-crates/issues/157), [#159](https://github.com/srinji-kaggss/logicalworks-crates/issues/159) |
| Every selected capability has a usable public import path downstream | unexercised-gap | Declared-but-unexported `bevy_app`/`bevy_time`/`bevy_state` → [#169](https://github.com/srinji-kaggss/logicalworks-crates/issues/169) |

## Part 4 — closure

This matrix is **not** the closure condition. The tracker closes when its full
concept register is accounted for at the owning issues, which are:

**Gate correctness:** [#157](https://github.com/srinji-kaggss/logicalworks-crates/issues/157)
→ [#158](https://github.com/srinji-kaggss/logicalworks-crates/issues/158);
[#159](https://github.com/srinji-kaggss/logicalworks-crates/issues/159) proceeds
independently.

**Semantic correctness:** [#153](https://github.com/srinji-kaggss/logicalworks-crates/issues/153),
[#154](https://github.com/srinji-kaggss/logicalworks-crates/issues/154),
[#160](https://github.com/srinji-kaggss/logicalworks-crates/issues/160),
[#161](https://github.com/srinji-kaggss/logicalworks-crates/issues/161),
[#162](https://github.com/srinji-kaggss/logicalworks-crates/issues/162),
[#163](https://github.com/srinji-kaggss/logicalworks-crates/issues/163),
[#164](https://github.com/srinji-kaggss/logicalworks-crates/issues/164),
[#165](https://github.com/srinji-kaggss/logicalworks-crates/issues/165),
[#166](https://github.com/srinji-kaggss/logicalworks-crates/issues/166),
[#167](https://github.com/srinji-kaggss/logicalworks-crates/issues/167),
[#168](https://github.com/srinji-kaggss/logicalworks-crates/issues/168),
[#169](https://github.com/srinji-kaggss/logicalworks-crates/issues/169).
Every issue in both lists is closed as of 2026-10-05.

**Assurance workstreams X1–X4:** X1 (constant-time hashing) was measured and closed
by #275; X2 (fallible thread admission) was closed by #264 — the `task` row now
carries a typed admission result on both entry points and an owned, joinable
blocking pool; X3 (entropy failure meaning) was closed by #276: the refusal
carries the OS's own code, its portable `io::ErrorKind` and a kind that
distinguishes "no source on this target" from "the OS refused", and the output
buffer's state after a refusal is documented and tested rather than assumed. X4
(feature-isolated ergonomics), owned by #417: the `tests-std-per-feature-{a,b,c}`
lanes test each feature alone (at `d55ee4e22`, 370–431 tests per feature, all
passing), and `sign`, missing from every shard, joined shard b. `process` refuses
off Unix with `Unsupported`; `trace` installs no global subscriber.
**assurance-gap** rows stay marked until their owners close.

**Nothing here converts an unresolved promise into a green checkbox:** a gap
row is a real, named, owned gap, shown rather than rounded up.