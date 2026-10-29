# lgwks_deps

A third-party dependency storefront, with an admission gate that audits what the
storefront is for.

The storefront packages each third-party stack as an optional cargo feature, so
a consumer selects a capability rather than a crate. The gate then checks that
every external dependency authored by a workspace package names its semantic
owner, capability, source, requirement, allowed consumers, and allowed
dependency kinds. Code that needs a library outside `std` and `lgwks_std` does
not pass the gate until a human registers it in `contract/APPROVED.toml`.

## Install

The binary is the gate; the library is the storefront.

```sh
cargo install lgwks_deps    # the `lgwks-deps` binary
cargo add lgwks_deps        # the storefront library
```

No feature is on by default, so `cargo add lgwks_deps` pulls no third-party
crate. The Rust source detectors the CLI once carried behind a default `scan`
feature moved to `lgwks_ast` with the one Rust reader: run `lgwks-ast scan`
([ADR 0002](../../docs/adr/0002-scan-routed-through-lgwks-ast.md)).

## Storefront

Each feature exposes a reviewed upstream capability through the storefront, so
a consumer never declares it directly. Do not add `tokio`, `gpui`, or Bevy
crates directly; select the storefront feature and import its public path. The
explicit admission gate detects unregistered direct edges when it runs; the
library does not intercept arbitrary Cargo invocations. Every feature, what it
re-exports, and what it pulls in is in
[`storefront.md`](../../docs/guides/lgwks-deps/storefront.md); the admission
records are [Candle](../../docs/candle-admission.md),
[Bevy and GPUI](../../docs/bevy-admission.md) and
[AppCUI](../../docs/appcui-admission.md).

## Gate

```sh
lgwks-deps check .              # audit this repository
lgwks-deps tiers                # the admission ladder, lowest rung first
lgwks-deps request <crate> <v>  # print an approval block to fill in
lgwks-deps init [PATH]          # fail-closed starting register
lgwks-deps freshness [PATH]     # resolved versus latest on crates.io
lgwks-deps vendor check [PATH]  # prove the lockfile is covered by the shared vendor tree
lgwks-deps debug [PATH]         # doctor the default lgwks_std debugger surface
lgwks-deps sqlite <PATH> [TARGET [SQL ...]]  # open with estate defaults; report or migrate
```

`check` runs as the first CI lane. The binary diagnoses; it never approves.
Approval is a committed diff with a human's name on it, and the register format
is documented in `contract/APPROVED.toml`.

`[policy] enforce = false` is an adoption posture, **not an off switch**. It is
a supported posture over a tree the gate admits: `check` still reports every
refusal it finds, and a tree with no refusals still exits 0. What it no longer
does is make a violating tree pass. A register that reads `enforce = false`
while the tree actually carries dependency-edge violations is itself refused, by
a named `AdoptionModeRefusals` refusal that states how many violations the
posture stood down, and `check` exits 2 — identically to `enforce = true`. The
verdict is therefore a function of the refusals alone; flipping that one token
cannot change a build. See issue #204.

`vendor check` is the physical counterpart of the register: the register states
which edges are owned, a vendored tree states which bytes an offline build
resolves, and the subcommand binds the two by hash. **Status in this repository:**
no `vendor/` tree is tracked (`git ls-files vendor` lists nothing) and
`.cargo/config.toml` names no `[source.vendored-sources]`, so no build resolves
from one and `lgwks-deps vendor check .` refuses with "names no
[source.vendored-sources] directory". The subcommand is for a repository that
vendors; nothing here makes repositories share one physical copy.

`tier` is read in exactly one place: an approval at the `vendor` tier whose
authored edge still resolves from a registry or git source is refused by name as
`VendorTierConflict`. That is the whole of the control. The gate does not prove a
`boundary` approval is not a vendored crate, and it does not review anything.

`debug` is the cargo-doctor path for observability: install the default
`lgwks_std::trace` debugger, emit lifecycle events, and refuse if the SDK
debugger no longer rides the default trace surface. `--json` writes the report
to stdout and leaves trace events on stderr.

`freshness` asks crates.io's sparse index about every locked package, 32 at a time
(`--jobs N`, `--index URL`). A pre-release is offered only to a lock already on
one, a yanked version never, and each row names the direct dependency it comes
through (`via`). Exit 0 is current, 1 behind, 2 the check did not finish.

### Register syntax

`contract/APPROVED.toml` and `contract/INVARIANTS.toml` use the same strict
TOML subset. Only the register's fixed headers and schema keys are accepted.
String values are single-line, double-quoted basic strings; TOML basic escapes
and Unicode scalar escapes are decoded, while any decoded control character is
refused. Literal and multiline strings, bare string values, trailing tokens,
and non-string values outside `[policy] enforce = true|false` are refused.
Comments outside strings and CRLF line endings are supported. `allowed_*`
fields are comma-separated package identifiers or edge kinds: a single final
comma remains a supported legacy terminator, while empty members and unknown
kinds are refused. Package and owner identifiers use Cargo's ASCII
letter/digit/hyphen/underscore spelling, and `source` is `registry`, `git`, or
`path`. Dates must be real Gregorian `YYYY-MM-DD` dates. Repeated keys refuse
with the entry identity and both source lines; valid unique keys are
order-independent.

An approval with no authored Cargo edge is refused as stale authority. An
authored edge with no approval is refused as unregistered. Both directions are
enforced.

## The other crates

Logical Works Incorporated publishes `lgwks_deps` as one of six independently versioned crates in [logicalworks-crates](https://github.com/srinji-kaggss/logicalworks-crates#readme): `lgwks_std`, `lgwks_bot`, `lgwks_ast`, `lgwks_macros`, `lgwks_deps` and `drishti`.
[Should I use logicalworks-crates?](https://github.com/srinji-kaggss/logicalworks-crates/blob/HEAD/docs/should-i-use.md) says when to reach for each one and when to skip it.

## License

Apache-2.0 — Copyright 2026 Logical Works Incorporated
