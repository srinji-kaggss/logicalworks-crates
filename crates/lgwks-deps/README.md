# lgwks_deps — dependency admission, audit, and freshness

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
```

The binary diagnoses; it never approves. Approval is a committed diff with a
human's name on it. See `contract/APPROVED.toml` for the register format.
