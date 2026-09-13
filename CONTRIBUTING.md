# Contributing — logicalworks-crates

## First: the deps law

Read `AGENTS.md` before adding any dependency. The default answer to "can I
add this crate?" is **no**: if the capability exists in `std`, `lgwks_std`,
`lgwks_bot`, or `lgwks_ast`, use it. A genuinely new third-party dependency
becomes an optional, feature-gated edge of `lgwks_deps` with
`owner = "lgwks_deps"`, registered in `contract/APPROVED.toml` — never a
direct edge, never a new top-level crate. `lgwks-deps check` refuses both
unregistered edges and stale unused approvals.

## Workflow

1. Branch from `main`. One concern per branch.
2. Follow `skills/lgwks-std-first/SKILL.md` before writing (check the estate
   surface first) and `skills/lgwks-dependency-admission/SKILL.md` if step 1
   genuinely fails.
3. Update `CHANGELOG.md` under `[Unreleased]` — per-crate Added / Changed /
   Decisions entries. A behavior or message change without a changelog entry
   is incomplete.
4. Keep every `pub mod` listing documented (`///` on the declaration line) —
   parent listings render empty otherwise.
5. New public API ships with a `/// ```rust` doc example so
   `cargo test --doc` compiles it, or an example under `examples/`.
6. Run the full gate before pushing:

```sh
cargo test --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all -- --check
cargo run -p lgwks_deps -- check .
./scripts/lgwks-std-package-smoke.sh
```

## Standards

- `#![forbid(unsafe_code)]` everywhere (workspace lints); no `unwrap` outside
  tests (CI enforces); typed errors at library boundaries with `source()`
  forwarding where a wrapped error exists.
- Match the file you are in: naming, comment density, idioms. Comments state
  constraints the code cannot show — no narration, no history, no reassurance.
- `0.x` means minors may break: bump the crate version, say so in the
  changelog, and never match on error strings across crates.
