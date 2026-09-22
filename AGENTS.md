# AGENTS.md

Four files are the governance surface, and each owns exactly one concern:

| File | Concern |
|---|---|
| [`GOVERNANCE.md`](GOVERNANCE.md) | Authority, laws, gates, the completion ledger, and what is not claimed |
| [`CODEBOOK.md`](CODEBOOK.md) | Code rules: lint contract, anti-slop invariants, API shape, tests |
| [`WORKFLOW.md`](WORKFLOW.md) | Process: local CI, the delivery loop, evidence proportionality, agent economics |
| `AGENTS.md` (this file) | **Dependency doctrine** — the narrower rule layered on `CODEBOOK.md` §10 |

A second copy of a policy is a second policy, and the two drift. Do not
duplicate these rules elsewhere; link here. Where this file and `CODEBOOK.md`
conflict on a dependency question, this file wins.

The gate is defined once in `scripts/gate-lanes.toml`. Run it with
`./scripts/ci-local.sh`. `scripts/check-gate-parity.py` keeps
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
- `lgwks_deps`: everything else, the **storefront**. The end user installs
  `lgwks_deps` and selects which dependency features to turn on.
- `lgwks_ast`: standalone, and finished. It is complete rather than parked, so
  it needs no further work and is not a candidate for new capability; do not
  grow it into a fourth surface.

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
   are default-off; the reviewed `scan` gate-tool feature is the explicit
   default-on exception.
3. A new dependency is registered under `lgwks_deps`. Do not introduce a new
   owner crate or a new top-level crate for third-party code.
4. Grandfathered: `lgwks_std`'s existing core feature stack and `lgwks_ast`'s
   parser. `lgwks_std` cannot route through `lgwks_deps`, because `lgwks_deps`
   depends on `lgwks_std` and the reverse would be a cycle.

Do **not** add `tokio`, `futures`, `async-trait`, `pollster`, `syn`,
`proc-macro2`, `regex`, `uuid`, `chrono`, `walkdir`, `glob`, `base64`, `hex`,
`percent-encoding`, `serde_json`, `ureq`, `reqwest`, or `ast-grep-*` directly.
Each maps to a workspace path; see `docs/dependency-doctrine.md`. Async and
runners are exposed by `lgwks_bot`; its tokio engine is selected through the
`lgwks_deps` storefront, which owns the workspace's one `tokio` edge. The gate
refuses a second `tokio` consumer.

## Before you reach for a crate

1. Read `docs/dependency-doctrine.md`, which covers the decision flow and the
   full replacement matrix, including the boundaries where no replacement
   exists.
2. Confirm the crate's rung: `cargo run -p lgwks_deps -- tiers`.
3. If the capability is genuinely missing, follow
   `skills/lgwks-dependency-admission/SKILL.md` exactly.
4. To check existing code against the policy before writing,
   `skills/lgwks-std-first/SKILL.md` is the operational map.

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
```

`lgwks-deps check` is the first CI job. A green compile with an unregistered
edge is a refused build, not a passing one.

`./scripts/doc-lanes.sh` is the rustdoc gate, and it was the one check reachable
only from CI. A broken intra-doc link is a warning rather than an error, so the
build fails only under `RUSTDOCFLAGS='-D warnings'`, and *which* links break
depends on which features are on. The script runs the four lanes the Docs job
runs — every feature, no features, the default set, and each `lgwks_std` feature
alone — and takes about a minute. Each lane has caught a break the other three
could not see: `trace` linking to `json`, `retry` to `random`, `wire` to `json`.
Pass `all-features`, `no-default-features`, `default` or `per-feature` to run
one of them.

`scripts/check-std-first.py` is the other half of the same rule, and it reads
the source rather than the manifest. `lgwks-deps check` sees every authored
edge; it cannot see a `use` of a crate nobody declared, which builds only
because something else in the graph re-exports it, or a capability hand-rolled
beside the `lgwks_std` module that already provides it. Every crate the source
reaches past `std` and the four surfaces is printed with the record that
approves it, so the answer to "why is this edge here" is in the output rather
than in a reviewer's memory. Run it with `--justify` to see those records
whether or not there is a finding.

`python3 scripts/check-doc-citations.py` pins the text of every line a guide
cites, so a citation that lands on another plausible line after a file grows is
a failure rather than a green page pointing at unrelated code. `--update`
records a legitimate move and prints every line whose text changed under a
citation, which is the review.

## What the gate enforces

INV-DEP-EDGE-OWNED: every external dependency authored by a workspace package
names its semantic owner, capability, source, requirement, allowed consumers,
and allowed dependency kinds. The register is `contract/APPROVED.toml`; the
gate is `crates/lgwks-deps`. The three dependency surfaces and the standalone
AST ownership are described in `README.md`.
