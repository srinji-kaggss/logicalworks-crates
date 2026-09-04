# logicalworks-crates

Source of truth for the estate's shared Rust foundation crates, extracted from
[Braid](https://github.com/srinji-kaggss/Braid) with versions unchanged:

| Crate | Version | Role |
|---|---|---|
| `lgwks_std` | 0.5.2 | Core functions: hex, time, id, hash, glob, pattern, json, ron, wire, fs, task, leb128, encoding |
| `lgwks_bot` | 0.1.2 | Async + actor roles: Observe, Evaluate, Execute, Query verbs over capability-gated domains |
| `lgwks_deps` | 0.1.0 | All other deps: admission, audit, freshness (`lgwks-deps check`) — not published |

All three crates are Apache-2.0, Logical Works Incorporated. Versions move
together from this repo; the next bump publishes here.

## Consumption contract (three lanes)

1. **Core functions → `std` + `lgwks_std`.** If the capability exists in
   `lgwks_std`, depending on the upstream crate directly is duplication, not
   choice — it becomes an `lgwks_std` module (CONSOLIDATE) or is removed
   (ELIMINATE).
2. **Async + actor roles → `lgwks_bot`.** Capability-gated `(condition,
   action)` chains over the shipped domains. A bot missing a grant fails at
   build time, not at runtime.
3. **Everything else → `lgwks_deps`-registered.** Any new dependency must be
   registered in `contract/APPROVED.toml` (owner, capability, source,
   requirement, consumers, kinds) and imported through the owning facade.
   `lgwks-deps check` refuses unregistered edges, drifted requirements or
   sources, and stale unused approvals — that is the duplication protection.

## Checks

```sh
cargo test --workspace --all-targets
cargo run -p lgwks_deps --bin lgwks-deps -- check .
./scripts/lgwks-std-package-smoke.sh
```
