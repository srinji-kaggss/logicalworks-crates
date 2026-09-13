# Security policy — logicalworks-crates

## Attack surface in scope

These crates parse untrusted bytes and open sockets: `lgwks_std::json` /
`ron` / `wire` deserialization, `lgwks_std::http` URL handling and response
bodies, `lgwks_ast` tree-sitter parsing of hostile source files,
`lgwks_bot::spec` manifest parsing with a 1 MiB cap. Documented boundaries:

- Error diagnostics never echo untrusted URLs or field names raw (log-forging
  refusal is enforced in code, not convention).
- `lgwks_ast` refuses oversized sources, over-budget trees, and recovery nodes
  before consumers see them; the unchecked `parse` escape hatch is for
  diagnostics and tests only.
- `lgwks_bot` authority is explicit and auditable, not a sandbox: in-process
  code can always dial out directly.

Out of scope for these crates (see `docs/distributed-boundaries.md`): mTLS /
private CA handling, key lifecycle, tracing/metrics facades, consensus.

## Reporting

Report vulnerabilities privately to the repository owner via a GitHub private
vulnerability report on
[srinji-kaggss/logicalworks-crates](https://github.com/srinji-kaggss/logicalworks-crates).
Do not open a public issue for a suspected vulnerability.

## Response

- Acknowledgement within 3 business days (ESTIMATE: maintainer best effort,
  single-maintainer project).
- Fix lands as a patch release with a `CHANGELOG.md` entry naming the
  affected versions; reporters are credited unless they decline.
- Supported versions: the latest published minor of each crate. `0.x` minors
  may break; pin exact versions in production manifests.
