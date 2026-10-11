# lgwks_deps: the register and the vendor tree

How `contract/APPROVED.toml` is written and read, and how `vendor check` binds it
to bytes. Moved from the crate README under the size law (#408); the CLI is in
[`index.md`](index.md).

## Register syntax

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

## Vendor tree and tiers

`vendor check` is the physical counterpart of the register: the register states
which edges are owned, a vendored tree states which bytes an offline build
resolves, and the subcommand binds the two by hash. **Status in this repository,
unresolved:** `vendor/` is present (294 directories, 12,240 tracked files) but
`.cargo/config.toml` names no `[source.vendored-sources]`, so no build resolves
from it and `lgwks-deps vendor check .` refuses with "names no
[source.vendored-sources] directory". Nothing here makes repositories share one
physical copy. The tree is outside all package directories, so `cargo package`
never ships it.

`tier` is read in exactly one place: an approval at the `vendor` tier whose
authored edge still resolves from a registry or git source is refused by name as
`VendorTierConflict`. That is the whole of the control. The gate does not prove a
`boundary` approval is not a vendored crate, and it does not review anything.
