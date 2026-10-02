# lgwks_deps: library and CLI

There are two ways to use this crate, and they have different dependency costs.

```sh
cargo install lgwks_deps                        # the `lgwks-deps` binary
cargo add lgwks_deps --no-default-features      # the storefront library
```

The binary is the gate. The library is the storefront. Current version:
`0.4.0`, MSRV Rust 1.98.0, edition 2024.

Do not run `cargo install lgwks_deps --no-default-features` expecting the `scan`
subcommand. The CLI's source detectors live behind the `scan` feature, which is
the default precisely so an install with no flags works.

## `--no-default-features` for library consumers

The default build carries `scan`, which pulls `syn` and `proc-macro2` for the
CLI's Rust source detectors. That is a Rust parser in your dependency graph, and
a runtime library has no use for it.

| What you are doing | Command | `syn` in your graph |
|---|---|---|
| Running the gate, including `lgwks-deps scan` | `cargo install lgwks_deps` | yes, in the tool |
| Embedding `check_dependencies` | `cargo add lgwks_deps` | yes, because the default is on |
| Embedding `check_dependencies`, leaner | `cargo add lgwks_deps --no-default-features` | no |
| Choosing a third-party stack | `cargo add lgwks_deps --no-default-features --features tokio` | no |

`scan` gates one module (`crates/lgwks-deps/src/lib.rs:79`) and the CLI's source
detectors, and nothing else. `check_dependencies` is not behind it: the embed
example further down runs against a `default-features = false` build. If you are
here for the storefront, disable defaults and select the engine you want.

## Storefront features

Each feature exposes its reviewed upstream capability through the storefront, so
you never declare it directly. `cargo add tokio` and `cargo add gpui` are refused
by the gate as second edges.

| Feature | Re-exports | Default |
|---|---|---|
| `tokio` | `lgwks_deps::tokio` | off |
| `tokio-time` | `lgwks_deps::tokio::time` | off |
| `tokio-sync` | `lgwks_deps::tokio::sync` | off |
| `tokio-macros` | `lgwks_deps::tokio` | off |
| `tokio-io` | `lgwks_deps::tokio::io` | off |
| `tokio-net` | `lgwks_deps::tokio::net` | off |
| `tokio-process` | `lgwks_deps::tokio::process` | off |
| `tokio-fs` | `lgwks_deps::tokio::fs` | off |
| `tokio-signal` | `lgwks_deps::tokio::signal` | off |
| `tokio-full` | `lgwks_deps::tokio` | off |
| `bevy-ecs` | `lgwks_deps::bevy_ecs` | off |
| `bevy-app` | `lgwks_deps::bevy_app` | off |
| `bevy-time` | `lgwks_deps::bevy_time` | off |
| `bevy-state` | `lgwks_deps::bevy_state` | off |
| `gpui` | `lgwks_deps::gpui` | off |
| `appcui` | `lgwks_deps::appcui` | off |
| `ml-candle` | `candle_core`, `candle_nn`, `candle_transformers` | off |
| `ml-candle-metal` | as `ml-candle`, plus Candle's macOS Metal backend | off |
| `ml-tokenizers` | `lgwks_deps::tokenizers` | off |
| `process-group-probe` | `lgwks_deps::process_group::exists` (Unix only) | off |
| `scan` | the gate's Rust source detectors | on |

The facade-only consumer matrix compiles GPUI examples on Linux, macOS, and
Windows, Candle Metal only on macOS, and the process-group probe only on Unix.
It does not treat those platform-bound combinations as all-target features.

For `tokio-macros`, alias `lgwks_deps::tokio` as `tokio` at the consumer crate
root before using Tokio's `#[tokio::main]` or `#[tokio::test]` macro.

```toml
[dependencies]
lgwks_deps = { version = "0.4.0", default-features = false, features = ["tokio"] }
```

The ML features are default-off, so the default `scan` build compiles none of
Candle's closure. Selecting `ml-candle` pulls `hf-hub` transitively through
`candle-transformers`, which makes the compiled tree network-capable even though
this repository's runtime loads a local checkpoint and does not call the hub.
`docs/candle-admission.md` records the authority and the transitive surface.
`gpui` is likewise outside the default build; `docs/bevy-admission.md` and
`docs/appcui-admission.md` cover the other two.

If you want the tokio engine behind the bot facade, use `lgwks_bot::rt` rather
than `lgwks_deps::tokio`. The storefront path is for the case where you are
deliberately working below that facade.

## The CLI

```sh
lgwks-deps check .              # audit this repository
lgwks-deps tiers                # the admission ladder, lowest rung first
lgwks-deps request <crate> <v>  # print an approval block to fill in
lgwks-deps init [PATH]          # fail-closed starting register
lgwks-deps freshness [PATH]     # resolved versus latest on crates.io
lgwks-deps vendor check [PATH]  # prove the lockfile is covered by the shared vendor tree
lgwks-deps scan [PATH]...       # source detectors, one verdict binary
lgwks-deps debug [PATH]         # doctor the default lgwks_std debugger surface
```

`check` reads `cargo metadata --no-deps` rather than the transitive lockfile
closure, because only metadata preserves which package authored an edge
(`crates/lgwks-deps/src/lib.rs:15`). It refuses in both directions: an approval
with no authored Cargo edge is stale authority, and an authored edge with no
approval is unregistered.

`vendor check` binds the register to the bytes the offline build resolves, by
hash.

`debug` is the cargo-doctor path for the debugging lifecycle. It installs the
default `lgwks_std::trace` debugger in its own process, emits lifecycle events,
and inspects the repository manifest to prove the default `trace` surface still
includes `tracing-subscriber`. `--json` keeps the report on stdout and the
trace stream on stderr so agents can parse it without scraping terminal text.

The binary diagnoses. It never approves. Approval is a committed diff in
`contract/APPROVED.toml` with a human's name on it.

## Embedding the gate

```rust
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let ((contract, refusals), unresolved) =
        lgwks_deps::check_dependencies(Path::new("."))?.into_parts();
    if let Some(cleanup) = unresolved {
        eprintln!("warning: {cleanup}");
    }
    println!(
        "{} approved edges, {} refusals",
        contract.entry_count(),
        refusals.len()
    );
    Ok(())
}
```

`check_dependencies(root)` returns
`Result<metadata::Collected<(Contract, Vec<Refusal>)>, GateError>`. An empty
refusal list is a pass. `Collected` carries the verdict and, beside it, any Cargo
capture file that could not be removed after a complete metadata read: the graph
was read in full, so the leftover file is a warning to report, not a reason to
discard the verdict. A
`GateError` is a different thing from a refusal: the module documentation lists
a missing register, unparseable metadata, an unparseable register, and an
unreadable lock file as errors, and all four are fail-closed, because "a gate
that passes when it cannot find its own contract is a gate that reports success
for the one condition it exists to catch."

`Refusal` is `#[non_exhaustive]` and carries typed payloads rather than strings:
`UnregisteredEdge` names the consumer, the crate, the requirement, the source,
and the dependency kind; `ForeignWorkspaceMember` names the declared repository
and the admitted one; `ConsumerNotAllowed` names the package that declared an
edge it was not allowed to declare.

The escape hatch is `enforce = false` under `[policy]` in the register itself, a
reviewable diff carrying a human's name. It is not an environment variable, so a
build cannot stand the gate down for itself.

## Adopting the policy is a separate decision

Using `lgwks_bot`, `lgwks_std`, or `lgwks_ast` in your application does not
require adopting this gate. Those crates are libraries; the dependency policy in
[`../../AGENTS.md`](../../AGENTS.md) is how *this repository* is built.

Reach for `lgwks_deps` when you want the same property in your own workspace:
every external edge named by a semantic owner, verified by a command that fails
closed. That is a larger commitment than adding a library, and it is worth
starting with `lgwks-deps init` and a single CI job rather than a wholesale
freeze.
