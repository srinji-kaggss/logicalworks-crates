# AGENTS.md

> **Read `INVARIANTS.md` before the first edit.** It is the short, enforced list of this repo's domain rules; this file is the long-form context.

Six files are the governance surface, and each owns exactly one concern:

| File | Concern |
|---|---|
| [`GOVERNANCE.md`](GOVERNANCE.md) | Authority, laws, gates, the completion ledger, and what is not claimed |
| [`CODEBOOK.md`](CODEBOOK.md) | Code rules: lint contract, anti-slop invariants, API shape, tests |
| [`WORKFLOW.md`](WORKFLOW.md) | Process: local CI, the delivery loop, evidence proportionality, agent economics |
| [`INVARIANTS.md`](INVARIANTS.md) | Short enforced rule list, read before the first edit |
| [`REQUIREMENTS.md`](REQUIREMENTS.md) | Immutable product requirements, superseded never edited |
| `AGENTS.md` (this file) | **Dependency doctrine** — the narrower rule layered on `CODEBOOK.md` §10 |

A second copy of a policy is a second policy, and the two drift. Do not
duplicate these rules elsewhere; link here. Where this file and `CODEBOOK.md`
conflict on a dependency question, this file wins.

The gate is defined once in `scripts/gate-lanes.toml`. Run it with
`./scripts/ci-local.sh`. The `check-gate-parity` binary of `lgwks_deps` keeps
`.github/workflows/ci.yml` in agreement with that table.

---

Read this before adding a dependency to any crate in this repository. The
workspace compiles against `std` and `lgwks_std` first; every other edge is a
registered decision, not an import line. The default answer to "can I add this
crate?" is no, and the decision is recorded in `contract/APPROVED.toml`.

## The rule

There are three dependency surfaces and one standalone parser:

- `lgwks_std`: the core surface.
- `lgwks_bot`: async, runners, and the actor roles.
- `lgwks_deps`: everything else, the **storefront**. The end user installs it
  and holds every feature that builds on their target; selecting is an opt-out.
- `lgwks_ast`: standalone, and the one parser. It reads source in many
  languages through tree-sitter and, through its default `script` feature,
  reads the `script!` language (`lgwks_ast::script::parse`), which
  `lgwks_macros` compiles and every tool reads through (SL-2, the Director's
  direction of 2026-10-08, #383). It is not a candidate for other capability;
  do not grow it into a fourth surface.

If a capability exists in `std`, `lgwks_std`, `lgwks_bot`, or `lgwks_ast`, use
it. A **new** third-party dependency is an optional, feature-gated edge of
`lgwks_deps`, registered in `contract/APPROVED.toml` with `owner =
"lgwks_deps"`, and on by default unless ADR 0003 records its cost. `lgwks-deps
check .` refuses every authored external edge with no owner, and refuses an
approval with no authored edge.

## The dependency policy (2026-09-12)

1. The surfaces are `std`, `bot`, and `deps`; `ast` is the one standalone crate.
2. `lgwks_deps` is the install-and-select storefront: every other dependency is
   an optional feature there, on by default where the target can carry it and
   the measured cost is recorded in ADR 0003 (today: `gpui`, the `ml-*` set).
3. A new dependency is registered under `lgwks_deps`. Do not introduce a new
   owner crate or a new top-level crate for third-party code.
4. Grandfathered: `lgwks_std`'s existing core feature stack and `lgwks_ast`'s
   parser. `lgwks_std` cannot route through `lgwks_deps`, because `lgwks_deps`
   depends on `lgwks_std` and the reverse would be a cycle.

Do **not** add `tokio`, `futures`, `async-trait`, `pollster`, `syn`,
`proc-macro2`, `regex`, `uuid`, `chrono`, `walkdir`, `glob`, `base64`, `hex`,
`percent-encoding`, `serde_json`, `ureq`, `reqwest`, or `ast-grep-*` as a *new*
edge. Read that as a ban on additions, not as a description of the tree:
`lgwks_std` and `lgwks_ast` already author several of these names directly
(`regex`, `serde_json`, `ureq` and `ast-grep-*` among them) under the
grandfather clause above. A listed name maps to a workspace path *when a facade
supplies it*; where none does, the approved direct edge is legal and removing it
would be the defect (`docs/dependency-doctrine.md` says which is which).

## Before you reach for a crate

1. Read `docs/dependency-doctrine.md`: the decision flow and the full
   replacement matrix, including where no replacement exists.
2. Confirm the crate's rung: `cargo run -p lgwks_deps -- tiers`.
3. If the capability is genuinely missing, follow
   `skills/lgwks-dependency-admission/SKILL.md` exactly.
4. To check existing code against the policy before writing,
   `skills/lgwks-std-first/SKILL.md` is the operational map.

## Checks that must pass

The lane table is the list; this file keeps no second copy of its commands.

```sh
./scripts/ci-local.sh                  # every lane of scripts/gate-lanes.toml
./scripts/ci-local.sh --fast           # the lanes marked fast
./scripts/ci-local.sh --lane std-first # one lane, as CI runs it
```

`lgwks-deps check` (lane `deps-register`) is the first CI job. A green compile
with an unregistered edge is a refused build, not a passing one.

`./scripts/doc-lanes.sh` is the rustdoc gate: a broken intra-doc link is only a
warning, so it fails under `RUSTDOCFLAGS='-D warnings'`, and *which* links break
depends on the features on. It runs every feature, no features, the default set
and each `lgwks_std` feature alone.

The `check-std-first` binary (lane `std-first`) is the other half of the same
rule: it reads the source, not the manifest, so it sees a `use` of an undeclared
crate that builds only because the graph re-exports it, and a capability
hand-rolled beside the `lgwks_std` module that provides it. `--justify` prints
the record that approves every crate the source reaches past the four surfaces.

The `check-doc-citations` binary (lane `doc-citations`) pins the text of every
line a guide cites, so a citation that lands on another line after a file grows
fails. `--update` records a legitimate move and prints every line whose text
changed under a citation, which is the review.

## What the gate enforces

INV-DEP-EDGE-OWNED: every external dependency authored by a workspace package
names its owner, capability, source, requirement, consumers and kinds in
`contract/APPROVED.toml`; the gate is `crates/lgwks-deps`. `README.md` describes
the three surfaces and the standalone AST ownership.
