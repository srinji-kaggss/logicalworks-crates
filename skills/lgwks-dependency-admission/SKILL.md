---
name: lgwks-dependency-admission
description: Use when adding, upgrading, or auditing a third-party dependency in a LogicalWorks estate repo, when `lgwks-deps check` refuses an edge (unregistered edge, consumer not allowed, requirement/source drift, kind not allowed, unused approval), or when deciding whether a crate belongs in lgwks_std, lgwks_bot, lgwks_ast, or the register. Covers the admission ladder, the APPROVED.toml schema, and the fail-closed rules.
---

# Adding a dependency to a gated LogicalWorks repo

The register is `contract/APPROVED.toml`; the gate is `lgwks-deps`. A build
cannot carry an external edge that is not in the register, and the register
cannot carry an approval with no authored edge. Both directions are enforced.

Run every command from the repository root.

## 0. First, try not to add anything

Read `docs/dependency-doctrine.md` and check the replacement matrix. If the
capability is in `std`, `lgwks_std`, `lgwks_bot`, or `lgwks_ast`, stop — using
the crate directly is duplication and will be refused or reverted. If it is a
general primitive with no equivalent, the correct answer is a new `lgwks_std`
module (ELIMINATE), not a consumer-local dependency.

## 1. Find the rung

```sh
cargo run -p lgwks_deps -- tiers
```

Lower rungs are preferred. Only `boundary` and `vendor` produce a register
entry; `eliminate` and `consolidate` produce a module in `lgwks_std` instead.

## 2. Print the form

```sh
cargo run -p lgwks_deps -- request <crate> <version>
```

This prints a fillable `[[approved]]` block. There is no command that approves
anything: approval is the committed diff with a human's name on it.

## 3. Fill every field — the register schema

Append the block to `contract/APPROVED.toml`. All twelve fields are required;
`contract.rs` refuses a missing field, an unknown key, a bad tier, a bad date,
and a reason that is not a real sentence.

```toml
[[approved]]
crate = "rustix"                 # package name exactly as Cargo.lock spells it
tier = "boundary"                # boundary | vendor
version = "^1"                   # EXACTLY the manifest requirement cargo metadata reports
owner = "lgwks_std"              # workspace crate responsible for the capability
capability = "fs.available_space" # stable semantic capability name
source = "registry"              # registry | git | path
allowed_consumers = "lgwks_std"  # comma-separated workspace crate names
allowed_kinds = "normal"         # normal, build, and/or dev
reason = "Filesystem capacity available to an unprivileged process is absent from the stable standard library."
approved_by = "Director"         # the human who decided
approved_on = "2026-09-12"       # ISO YYYY-MM-DD
review = "https://github.com/..." # path or URL to the evidence
```

Field rules the parser enforces:

- `version` must equal what `cargo metadata --no-deps` reports for the edge.
  `version = "1"` in the manifest is reported as `^1`; write `^1`. Drift is
  refused as `RequirementDrift`.
- `reason` is a sentence: at least four words, at least 24 characters, ending
  in a full stop, and not just the crate name. Say what `std` cannot do.
- `allowed_consumers` / `allowed_kinds` are comma-separated; a consumer not on
  the list is refused as `ConsumerNotAllowed`.
- A `registry` approval does not cover a `git` or `path` edge (`SourceDrift`).

## 4. Add the authored edge

Add the dependency to the owning crate's `Cargo.toml` under that owner's lane:

- Core primitive → `crates/lgwks-std` (its existing feature stack is
  grandfathered).
- Async, runners, actor roles → `crates/lgwks-bot`; its runtime engine is
  selected through the `lgwks_deps` storefront's `tokio` feature.
- Parsing → `crates/lgwks-ast` (standalone, grandfathered; do not add a second
  parser).
- **Everything else → an optional, default-off feature of `crates/lgwks-deps`**
  — the storefront. This is the default for a new dependency: register with
  `owner = "lgwks_deps"` and expose it as a feature such as
  `gpui = ["dep:gpui"]`. The default build pulls none of them.

The consumer in the manifest must match `allowed_consumers`, the kind must
match `allowed_kinds`, and the requirement must match `version`.

## 5. Update everything that pins the closure

An approval that is not matched by these is incomplete:

- `deps_are_approved_leaves` in `crates/lgwks-deps/src/lib.rs` — the `APPROVED`
  list for `lgwks_std` dependency names (and the separate exact list for the
  gate's own dependencies).
- The `## Dependency philosophy` bullets in `crates/lgwks-std/README.md` — CI
  `contract-drift` compares these to the manifest dependency keys and fails on
  any mismatch.
- The feature map in `crates/lgwks-std/src/lib.rs` and the feature table in
  `crates/lgwks-std/README.md`, if you added a feature.
- `.github/workflows/ci.yml` feature matrix, if you added a feature.

## 6. Prove it

```sh
cargo metadata --locked >/dev/null        # lockfile current
cargo run -p lgwks_deps -- check .        # must print OK
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --all-targets --locked
./scripts/lgwks-std-package-smoke.sh
```

`check` must print `OK ... every authored external edge is owned`. A refusal
names the crate, the requirement, and the source class, and each refusal is a
decision, not paperwork.

## 7. Read the refusals as instructions

| Refusal | Repair |
|---|---|
| `UnregisteredEdge` | Add the `[[approved]]` block and the authored edge. |
| `ConsumerNotAllowed` | Add the consumer to `allowed_consumers`, or move the edge to the owner. |
| `RequirementDrift` | Make `version` equal the manifest requirement (`^1`, not `1`). |
| `SourceDrift` | `source` must be `registry`, `git`, or `path` matching the edge. |
| `KindNotAllowed` | Add `normal`/`build`/`dev` to `allowed_kinds`. |
| `UnusedApproval` | An approval with no authored edge is stale — add the edge or delete the block. |
| `ForeignWorkspaceMember` | A workspace member declares a foreign repository; consume the published crate instead. |

## Hard rules

- The register is not a whitelist. A name with a reason, a pin, an approver, a
  date, and evidence is a contract; a bare name is refused.
- ELIMINATE and CONSOLIDATE crates never get a register entry.
- The gate is fail-closed: no register, unparseable register, or unreadable
  lock is a refusal, never a pass.
