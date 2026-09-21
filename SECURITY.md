# Security policy

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

## Transitive advisories assessed and not actioned

An advisory is a claim about a crate, not about the call paths a consumer
reaches. Where the flagged code is unreachable from this workspace's dependency
graph, it is recorded here with its evidence, so the position is checked rather
than silent. Each entry names what would reopen it.

### `grid`: GHSA-38c5-483c-4qqp, integer overflow in `Grid::expand_rows`

Reached only by enabling the `gpui` storefront feature of `lgwks_deps`
(default-off): `lgwks_deps` → `gpui 0.2.2` → `taffy 0.9.0` → `grid 0.18.0`.
Unreachable, on three checks:

1. `cargo tree -i grid` shows `taffy` is the only parent `grid` has in the graph.
2. `taffy 0.9.0` never calls a vulnerable function. Its entire use of the crate
   is `Grid::new`, `Grid::from_vec`, `Grid::get`, and `Grid::get_mut`
   (`src/compute/grid/types/cell_occupancy.rs`); the strings `expand_rows` and
   `expand_cols` do not appear anywhere in its source.
3. The two constructors it does call already use checked arithmetic in 0.18.0:
   `new_with_order` uses `rows.checked_mul(cols)`, `from_vec_with_order` uses
   `checked_div` behind an assert. The 1.0.1 hardening covers `expand_*`,
   `push_*`, `insert_*`, and `prepend_*`; taffy reaches none of them.

There is no version bump that resolves it: `gpui 0.2.2` is the newest gpui and
pins `taffy = "=0.9.0"`, while `taffy 0.9.0` requires `grid = "^0.18.0"` and the
fix ships only in `grid 1.0.1`. A `[patch.crates-io]` is not a fix here even in
principle. A patch is workspace-local and is not published, so it would clean
this repository's lockfile while every consumer still resolved the vulnerable
version. Dependabot alert #1 is dismissed as
`vulnerable_code_not_in_execution_path` on this evidence.

**Reopen if** `gpui` moves to a `taffy` that calls any `expand_*`, `push_*`,
`insert_*`, or `prepend_*` method, or if a newer `grid 0.18.x` or `taffy 0.9.x`
appears. Either would make the version bump available and this entry obsolete.

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
