# lgwks_deps: embedding the gate

## Embedding the gate

Call `lgwks_deps::check_dependencies(root)?.into_parts()`: it yields the
register and its refusals together with any cleanup warning, and a caller prints
`contract.entry_count()` and `refusals.len()` from them. The integration test
`the_embedding_flow_reads_the_register_and_reports_refusals` runs that flow over
this repository and holds the refusal list empty.

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

## One owner per check

The doc-citation, requirements and spec-owner checks each have exactly one
implementation in the estate, and it lives here: `contract/GATE_OWNERSHIP.toml`
names the check, its single file, its shared entry, its binary and its
designated library consumer, and the `gate_ownership` integration test refuses
a second file implementing any check's entry. The binaries (`check-doc-citations`,
`check-requirements`, `check-spec-owners`) are thin shims over the same entries
`logical_ci`'s `docs-refs` and `governance` gates call as a library, so a changed
document's dangling citations and an unowned requirement are refused under one
rule with one owner, not two rule sets drifting apart.

## One installable step

`lgwks-deps gate [PATH]` runs the checks that make the `script!` path bind in
order — admission, citations, requirements, owners, `script check`, clippy with
the flow `forbid` lints, and the architecture diff against `--base` — with one
summary and one exit code (`--json` for agents). A leg with no tool or no base
to run on is a named skip, never a pass. Consumers add the reusable workflow
`.github/workflows/consumer-gate.yml` (or the composite action
`.github/actions/lgwks-gate` directly); it installs the tools from crates.io
and runs the gate on GitHub-hosted runners. The `gate_consumer` integration
test drives the shipped binary against scratch consumers with a planted
unregistered edge, a refused flow and a widening change, and each fails the
step.

## Adopting the policy is a separate decision

Using `lgwks_bot`, `lgwks_std`, or `lgwks_ast` in your application does not
require adopting this gate. Those crates are libraries; the dependency policy in
[`../../AGENTS.md`](../../AGENTS.md) is how *this repository* is built.

Reach for `lgwks_deps` when you want the same property in your own workspace:
every external edge named by a semantic owner, verified by a command that fails
closed. That is a larger commitment than adding a library, and it is worth
starting with `lgwks-deps init` and a single CI job rather than a wholesale
freeze.
