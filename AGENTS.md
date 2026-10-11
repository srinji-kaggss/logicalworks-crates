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

A second copy of a policy is a second policy, and the two drift: link here.
Where this file and `CODEBOOK.md` conflict on a dependency, this file wins.
The gate is defined once in `scripts/gate-lanes.toml` and run with
`./scripts/ci-local.sh`; `scripts/check-gate-parity.py` keeps `ci.yml` in step.

---

Read this before adding a dependency to any crate in this repository. The
workspace compiles against `std` and `lgwks_std` first; every other edge is a
registered decision, not an import line. The default answer to "can I add this
crate?" is no, and the decision is recorded in `contract/APPROVED.toml`.

## The rule

There are three dependency surfaces and one standalone parser:

- `lgwks_std`: the core surface.
- `lgwks_bot`: async, runners, and the actor roles.
- `lgwks_deps`: everything else, the **storefront**. The end user installs
  `lgwks_deps` and selects which dependency features to turn on.
- `lgwks_ast`: standalone, and the one parser. It reads source in many
  languages through tree-sitter and, behind its default-off `script` feature,
  reads the `script!` language (`lgwks_ast::script::parse`), which
  `lgwks_macros` compiles and every tool reads through (SL-2, the Director's
  direction of 2026-10-08, #383). Behind `scan` it runs the zero-gate source
  detectors over its Rust grammar (`lgwks-ast scan`, #408). It is not a
  candidate for other capability; do not grow it into a fourth surface.

If a capability exists in `std`, `lgwks_std`, `lgwks_bot`, or `lgwks_ast`, use
it. A **new** third-party dependency is an optional, feature-gated edge of
`lgwks_deps`, never built unless the end user selects it, registered in
`contract/APPROVED.toml` with `owner = "lgwks_deps"`. `lgwks-deps check .`
refuses every authored external edge with no owner, and refuses an approval
with no authored edge.

## The dependency policy (2026-09-12)

1. The surfaces are `std`, `bot`, and `deps`; `ast` is the one standalone crate.
2. `lgwks_deps` is the install-and-select storefront: every other dependency is
   an optional feature there (`--features gpui`). Storefront capability features
   are default-off, with no exception; the source scan is `lgwks-ast scan`
   (#408), read through the one parser.
3. A new dependency is registered under `lgwks_deps`. Do not introduce a new
   owner crate or a new top-level crate for third-party code.
4. Grandfathered: `lgwks_std`'s existing core feature stack and `lgwks_ast`'s
   parser. `lgwks_std` cannot route through `lgwks_deps`, because `lgwks_deps`
   depends on `lgwks_std` and the reverse would be a cycle.

Do **not** add `tokio`, `futures`, `async-trait`, `pollster`, `syn`,
`proc-macro2`, `regex`, `uuid`, `chrono`, `walkdir`, `glob`, `base64`, `hex`,
`percent-encoding`, `serde_json`, `ureq`, `reqwest`, or `ast-grep-*` as a *new*
edge. It bans additions; it does not describe the tree. `lgwks_std` and
`lgwks_ast` author `regex`, `serde_json`, `ureq` and `ast-grep-*` directly under
the grandfather clause, so a listed name maps to a workspace path *when a facade
supplies it*, and where none does the approved direct edge is legal (see
`docs/dependency-doctrine.md`). Async and runners are `lgwks_bot`'s; its tokio
engine comes through the storefront, which owns the one `tokio` edge, and the
gate refuses a second `tokio` consumer.

## Before you reach for a crate

1. `docs/dependency-doctrine.md`: the decision flow and replacement matrix.
2. The crate's rung: `cargo run -p lgwks_deps -- tiers`.
3. A genuinely missing capability: `skills/lgwks-dependency-admission/SKILL.md`.
4. Existing code against the policy: `skills/lgwks-std-first/SKILL.md`.

## Checks that must pass

```sh
cargo test --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all -- --check
cargo run -p lgwks_deps -- check .
./scripts/lgwks-std-package-smoke.sh
./scripts/doc-lanes.sh
python3 scripts/check-std-first.py
python3 scripts/check-doc-citations.py
python3 scripts/check-requirements.py
```

`lgwks-deps check` is the first CI job. A green compile with an unregistered
edge is a refused build, not a passing one.

- `./scripts/doc-lanes.sh` is the rustdoc gate. A broken intra-doc link fails
  only under `RUSTDOCFLAGS='-D warnings'`, and which links break depends on the
  features on, so it runs the Docs job's four lanes (`all-features`,
  `no-default-features`, `default`, `per-feature`; pass one name to run it).
  Each has caught a break the others could not: `trace`→`json`,
  `retry`→`random`, `wire`→`json`.
- `scripts/check-std-first.py` reads the source, not the manifest: it sees a
  `use` of a crate nobody declared (built only through a re-export) and a
  capability hand-rolled beside the `lgwks_std` module that provides it. Every
  crate past `std` and the four surfaces prints with the record approving it;
  `--justify` prints those records even when clean.
- `scripts/check-doc-citations.py` pins the text of every cited line, so a
  citation that drifts onto another line fails. `--update` records a real move
  and prints each changed line under a citation, which is the review.

## What the gate enforces

INV-DEP-EDGE-OWNED: every external dependency authored by a workspace package
names its semantic owner, capability, source, requirement, allowed consumers,
and allowed dependency kinds. The register is `contract/APPROVED.toml`; the
gate is `crates/lgwks-deps`. The three dependency surfaces and the standalone
AST ownership are described in `README.md`.
