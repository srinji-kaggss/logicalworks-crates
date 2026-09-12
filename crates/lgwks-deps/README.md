# lgwks_deps — dependency admission, audit, freshness, vendor coverage, and source scan

`lgwks_deps` owns INV-DEP-EDGE-OWNED: every external dependency authored by a
workspace package names its semantic owner, capability, source, requirement,
allowed consumers, and allowed dependency kinds. Code that needs a library
outside `std` / `lgwks_std` does not compile until a human registers it in
`contract/APPROVED.toml`.

```sh
lgwks-deps check .              # audit this repo (first CI lane)
lgwks-deps tiers                # the admission ladder, lowest rung first
lgwks-deps request <crate> <v>  # print an approval block to fill in
lgwks-deps init [PATH]          # fail-closed starting register
lgwks-deps freshness [PATH]     # resolved vs latest on crates.io
lgwks-deps vendor check [PATH]  # prove the lockfile is covered by the shared vendor tree
lgwks-deps scan [PATH]...       # keel zero-gate detectors (swallow/unlogged/allow/chain/doc), one verdict binary
```

`vendor check` is the physical counterpart of the register: the register says
which edges are owned, the tree in `vendor/` says which bytes the offline
build resolves, and the subcommand binds them by hash. The tree lives at the
workspace root so every estate repo resolves one physical copy; it is outside
all package directories, so `cargo package` never ships it.

## Storefront features

Third-party capabilities are optional features selected by the consumer:

```toml
[dependencies]
lgwks_deps = { version = "0.1", features = ["gpui"] }
```

The `gpui` feature re-exports `lgwks_deps::gpui` and retains GPUI's normal
platform renderer. macOS builds require a usable Xcode Metal Toolchain
(`metal` and `metallib`); Linux builds require the platform development stack
selected by GPUI. The feature is not compiled by the default `scan` build.

The binary diagnoses; it never approves. Approval is a committed diff with a
human's name on it. See `contract/APPROVED.toml` for the register format.
