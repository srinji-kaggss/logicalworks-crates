# Governance §3 and §4

Part of [`GOVERNANCE.md`](../../GOVERNANCE.md), which indexes every section.

## 3. Gates

The gate is one definition: `scripts/gate-lanes.toml`. `./scripts/ci-local.sh`
executes the local lanes from it and CI executes the same lane commands, and
`scripts/check-gate-parity.py` refuses any drift between the two. They do not
run in the same order — CI fans lanes out across jobs — and a green local run
is evidence only for the lanes whose `surfaces` include `local`. The receipt
names what it did not cover. A disagreement is a defect in whichever side
diverged from the lane table.

| Gate | Command | Refusal condition |
|---|---|---|
| Lockfile | `cargo metadata --locked` | lockfile out of date |
| Dependency register | `cargo run -p lgwks_deps -- check .` | unregistered edge, stale approval, widened consumer, second `tokio` |
| Compile | `cargo test --workspace --all-targets --locked --no-run` | any rustc error code |
| Tests | `cargo test --workspace --all-targets --locked` | any failure |
| Feature matrix | per-crate `--all-features` / `--no-default-features` clippy | any warning |
| Lint | `cargo clippy --workspace --all-targets --locked -- -D warnings` | any warning |
| Format | `cargo fmt --all -- --check` | any diff |
| Doc build | `RUSTDOCFLAGS='-D warnings' cargo doc --no-deps` | broken link or warning |
| Requirements | `python3 scripts/check-requirements.py` | normative sentence changed with no Supersession log entry |
| Unwrap scan | no `.unwrap()` outside `mod tests` | any production unwrap |
| Package smoke | `./scripts/lgwks-std-package-smoke.sh` | smoke failure |
| Docs.rs metadata | declared feature set must build | unbuildable declaration |
| README pins | version pins track the manifests | stale pin |
| Lint-lint | control-lint validation on any `[lints]` edit | control did not warn |
| Suppressions | every `#[allow]`/`#[expect]` carries `reason` | a reasonless suppression |
| Artifacts | no `target/`, `graphify-out/`, `.lgwks/`, `node_modules/`, `.codegraph/` | any in the commit |

`gpui` and `ml-candle-metal` storefronts require the Xcode Metal toolchain and
are covered by the macOS lanes, not the Linux lanes. Their absence from a Linux
run is not a pass; it is a lane that did not run.

### Merge gate

Source review may proceed against local receipts. Merge eligibility requires
executed reproduction of the affected lanes on hosted CI. A bounded exception
requires explicit Director authorization recorded in the ledger below with its
replacement evidence enumerated. A local PASS claim alone is never a satisfied
execution gate.

Hosted Actions executes on this account and is the CI surface. The workflow
routes to `ubuntu-latest`, `macos-14`, and `windows-latest`, with `macos-14`
reserved for the Metal storefronts (`gpui`, `ml-candle-metal`). A lane whose
`platforms` exclude this host is `skip`, named in the receipt, and is never
counted as `pass`. Portable claims rest on the three-OS matrix actually
executing.

**Publishing is manual.** `crates.io` publish requires a human-held token. A
green dry-run is not evidence that publishing works. `docs/releasing.md` is the
release process; a release cut is a Director action.

---

## 4. Change control for governance

`GOVERNANCE.md`, `CODEBOOK.md`, `WORKFLOW.md`, `AGENTS.md`, `INVARIANTS.md`,
and `REQUIREMENTS.md` are the governance surface. Changing them is a
governance change and follows these rules:

1. A governance change ships in its own commit, separate from the semantic change
   it describes, unless the two are inseparable.
2. It records *why* in the ledger below, with the date and the decision it
   implements or supersedes.
3. It never weakens a gate to clear a refusal. Widening `allowed_consumers`,
   editing a pinned version, adding an `APPROVED.toml` row, or loosening a lint
   level to pass a check is a suppressed gate, and it is a defect.
4. It never turns a `forbid` into a `deny` except through the documented
   exceptions in `CODEBOOK.md` §2.1.
5. Retiring a document moves it to `docs/archive/` and registers the
   supersession here. There is never a second live copy.
6. This repository's dependency doctrine (`AGENTS.md`, `docs/dependency-doctrine.md`,
   `contract/APPROVED.toml`) is a *narrower* rule layered on top of `CODEBOOK.md`
   §10. Where they conflict on a dependency question, the narrower rule wins.
