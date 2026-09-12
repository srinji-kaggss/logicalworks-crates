# logicalworks-crates

Source of truth for the estate's shared Rust foundation crates, extracted from
[Braid](https://github.com/srinji-kaggss/Braid) and released from this workspace:

| Crate | Version | Role |
|---|---|---|
| `lgwks_std` | 0.6.1 | Core functions: hex, time, id, hash, glob, pattern, json, ron, wire, fs, task, leb128, encoding |
| `lgwks_bot` | 0.3.0 | Async, runner, and actor surface: Observe, Evaluate, Execute, Query verbs plus the curated runtime |
| `lgwks_ast` | 0.1.1 | The estate's one multi-language AST parser: language identification, ast-grep grammar selection, bounded traversal |
| `lgwks_deps` | 0.1.5 | The third-party storefront: opt-in dependency features, admission, audit, freshness (`lgwks-deps check`) |

All four crates are Apache-2.0, Logical Works Incorporated. Versions move
together from this repo; the next bump publishes here.

## The deps law

Cognitive load is three surfaces plus one grandfathered parser:

- **`lgwks_std`** — the std+ core.
- **`lgwks_bot`** — async, runners, and the actor roles.
- **`lgwks_deps`** — everything else: the **storefront**. Install it and select
  the dependency features you want; capability features are default-off. The
  reviewed `scan` gate-tool feature is the explicit default-on exception.
- **`lgwks_ast`** — standalone. Already adopted; grandfathered, not a lane to
  grow.

Every new third-party dependency is an optional, feature-gated edge of
`lgwks_deps` — e.g. `lgwks_deps = { version = "0.1", features = ["gpui"] }` —
registered with `owner = "lgwks_deps"`. `gpui` (Zed's GPU desktop UI framework)
is the first entry. `lgwks_std`'s core feature stack and `lgwks_ast`'s parser
are grandfathered: `lgwks_std` cannot route through `lgwks_deps`, because
`lgwks_deps` depends on `lgwks_std` and the reverse would be a cycle.

## Consumption contract

1. **Core functions → `std` + `lgwks_std`.** If the capability exists in
   `lgwks_std`, depending on the upstream crate directly is duplication, not
   choice — it becomes an `lgwks_std` module (CONSOLIDATE) or is removed
   (ELIMINATE).
2. **Async, runners, actor roles → `lgwks_bot`.** Capability-gated
   `(condition, action)` chains over the shipped domains; a bot missing a grant
   fails at build time, not at runtime.
3. **Multi-language parsing → `lgwks_ast`.** Language identification, grammar
   selection, and bounded tree traversal for code tools. Consumers do not
   depend on `ast-grep` directly; a language's grammar is a cargo feature, so
   each tool compiles exactly the languages it selects.
4. **Everything else → an `lgwks_deps` storefront feature.** Register the edge
   in `contract/APPROVED.toml` (owner, capability, source, requirement,
   consumers, kinds) and expose it as an optional feature. `lgwks-deps check`
   refuses unregistered edges, drifted requirements or sources, and stale unused
   approvals — that is the duplication protection.

## Checks

```sh
cargo test --workspace --all-targets
cargo run -p lgwks_deps --bin lgwks-deps -- check .
./scripts/lgwks-std-package-smoke.sh
```
