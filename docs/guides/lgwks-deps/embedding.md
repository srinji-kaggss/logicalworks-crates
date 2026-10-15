# lgwks_deps: embedding the gate

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
