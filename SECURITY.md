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
  code can always dial out directly. A flow's `Tenant` is its trusted host's
  partition key, not an authenticated identity; direct file, network and process
  paths from a flow are compile-time refusals, so I/O runs only through domains.

Out of scope for these crates (see `docs/distributed-boundaries.md`): key lifecycle
and rotation, tracing/metrics facades, consensus.

## Transitive advisories assessed and not actioned

An advisory claims something about a crate, not about the call paths this
workspace reaches. Where the flagged code is unreachable, the entry below
records the evidence and what would reopen it. The machine-readable half is
`.cargo/audit.toml` (`advisories.ignore` names these ids and nothing else);
an id silenced there with no section here is unchecked and fails review. The
`grid` entry below is a GitHub advisory with no RustSec id, so it cannot
appear there.

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

### `zip 8.6.0`: RUSTSEC-2026-0367, -0368, -0369, extraction writes outside the destination

Reached only by enabling the `ml-candle` storefront features of `lgwks_deps`
(default-off): `lgwks_deps` → `candle-core 0.11.0` → `zip 8.6.0`. Unreachable,
on three checks (2026-10-20):

1. `cargo tree -i zip@8.6.0 --all-features --target all` shows `candle-core` is
   the only parent `zip` has in the graph.
2. All three advisories are in `ZipArchive::extract` and
   `ZipStreamReader::extract`: SUID/SGID bits kept, symlink targets outside the
   destination, and permissions applied through symlinks.
3. `candle-core` never extracts. Its entire use of the crate is
   `ZipArchive::new`, `by_name`, `by_index` and `zip::result::ZipError`
   (`src/npy.rs`, `src/pickle.rs`, `src/error.rs`). It reads tensors into memory
   and writes nothing to disk.

There is no version bump that resolves it: `candle-core 0.11.0` is the newest
release and requires `zip` 8, and the fix ships only in `zip >= 9.0.0`.

**Reopen if** `candle-core` calls any `extract` method, or if a release
requiring `zip >= 9` appears; that bump makes this entry obsolete.

### Unmaintained transitive crates: ten default-off advisories behind `gpui` or Candle

No advisory here has a patched version, so no bump clears one. Each is absent
from the default graph and from every build that does not select the named
feature (assessed 2026-10-20); the lockfile stays where `cargo update` leaves it.

| Crates and versions | Advisory ids | Reached by | Reopen if |
|---|---|---|---|
| `rustybuzz` 0.14.1, 0.20.1; `ttf-parser` 0.20.0, 0.21.1, 0.25.1 | RUSTSEC-2026-0206, -0192 | `gpui` → `cosmic-text`, `usvg` → `fontdb`, `rustybuzz`, `ttf-parser` | `gpui` moves to a maintained text stack (`harfrust`), or a shipped binary enables `gpui` |
| `async-std` 1.13.2; `rustls-pemfile` 2.2.0 | RUSTSEC-2025-0052, -0134 | `gpui` → `gpui_http_client` → `zed-async-tar`, `zed-reqwest` | `gpui` ships, or its HTTP client leaves the Zed forks |
| `instant` 0.1.13; `proc-macro-error2` 2.0.1 | RUSTSEC-2024-0384, -0173 | `gpui` → `gpui_util` → `futures-lite` → `fastrand`; `gpui` → `stacksafe` → `stacksafe-macro` | `gpui` ships, or those parents drop them |
| `zbus_names` 4.3.4; `zvariant` 5.15.0; `zvariant_derive` 5.15.0 | RUSTSEC-2026-0371, -0370, -0372 | `gpui` → `ashpd`, `oo7` → `zbus 5.19.0` → these three | `gpui` moves to `zbus` 6 (where these are modules), or ships |
| `paste` 1.0.15 | RUSTSEC-2024-0436 | `gpui` → `image`, `metal`; `ml-candle` → `candle-core`, `tokenizers 0.22.2`; `ml-tokenizers` → `tokenizers 0.23.2` | any of the three features ships, or those parents drop `paste` |

### `rsa 0.10.0-rc.19`: RUSTSEC-2023-0071, Marvin timing sidechannel

Test-only `lgwks_std` dev edge for the #714 throwaway PKI
(`allowed_kinds = "dev"` in `contract/APPROVED.toml`): fresh keys per run,
loopback use in the same process, discarded after. No remote party observes
decryption timing; `cargo tree -i rsa` shows only test-target parents, so the
flagged path never ships. No fixed version exists, so no bump clears it.

**Reopen if** a patched `rsa` appears, or if any non-test target depends on it.

## Reporting

Report vulnerabilities privately to the repository owner via a GitHub private
vulnerability report on
[srinji-kaggss/logicalworks-crates](https://github.com/srinji-kaggss/logicalworks-crates).
Do not open a public issue for a suspected vulnerability.

## Response

- Acknowledgement within 3 business days (ESTIMATE: single-maintainer best effort).
- Fix lands as a patch release with a `CHANGELOG.md` entry naming the
  affected versions; reporters are credited unless they decline.
- Supported versions: the latest published minor of each crate's current
  major. A major release may break; pin the major in production manifests.
