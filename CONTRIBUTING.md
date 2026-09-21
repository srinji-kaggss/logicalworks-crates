# Contributing

## Dependency policy

Read [`docs/dependency-doctrine.md`](docs/dependency-doctrine.md) before adding a
dependency. The default answer to "can I add this crate?" is no. If the
capability exists in `std`, `lgwks_std`, `lgwks_bot`, or `lgwks_ast`, use it.

A new third-party dependency becomes an optional, feature-gated edge of
`lgwks_deps` with `owner = "lgwks_deps"`, registered in
`contract/APPROVED.toml`. It is never a direct edge and never a new top-level
crate. `lgwks-deps check` refuses both unregistered edges and stale unused
approvals.

## Workflow

1. Branch from `main`. One concern per branch.
2. Check the existing surface before writing new code. If the capability is
   genuinely missing, follow `skills/lgwks-dependency-admission/SKILL.md`.
3. Update `CHANGELOG.md` under `[Unreleased]`, with per-crate Added, Changed,
   and Decisions entries. A behaviour or message change without a changelog
   entry is incomplete.
4. Document every `pub mod` listing with a `///` on the declaration line;
   parent listings render empty otherwise.
5. New public API ships with a `/// ```rust` doc example so `cargo test --doc`
   compiles it, or an example under `examples/`.
6. Run the full gate before pushing.

```sh
cargo test --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all -- --check
cargo run -p lgwks_deps -- check .
./scripts/lgwks-std-package-smoke.sh
```

## Contributions and copyright

Three of the four crates are Apache-2.0 and take contributions on ordinary
terms. `lgwks_bot` is offered under two licences, including a commercial one,
and offering a work under two licences requires the licensor to hold sufficient
rights in every contribution to it.

**`lgwks_bot` is therefore closed to outside contributions.** A signed
contributor licence agreement would have to be in place before one could be
merged, none has been selected, and choosing one is deferred — so a patch sent
today has no path to merge and is refused on arrival rather than parked. The
reason is in [`LICENSING.md`](LICENSING.md), not a judgement about the patch.
Contributions to the other three crates are unaffected and welcome.

## Standards

- `#![forbid(unsafe_code)]` everywhere; no `unwrap` outside tests; typed errors
  at library boundaries, with `source()` forwarding where a wrapped error
  exists.
- Match the file you are in: naming, comment density, idioms. Comments state
  constraints the code cannot show. No narration, no history, no reassurance.
- `0.x` means minors may break. Bump the crate version, record it in the
  changelog, and never match on error strings across crates.
