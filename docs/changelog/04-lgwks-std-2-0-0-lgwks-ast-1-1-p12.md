# lgwks_std 2.0.0 / lgwks_ast 1.1.0 / lgwks_deps 3.0.0 / lgwks_macros 1.1.1 / lgwks_bot 2.0.0 - 2026-10-06 (12/20)

Release: lgwks_std 2.0.0 / lgwks_ast 1.1.0 / lgwks_deps 3.0.0 / lgwks_macros 1.1.1 / lgwks_bot 2.0.0 - 2026-10-06. Page 12 of 20. [Changelog index](../../CHANGELOG.md)

### lgwks_deps — a `cfg` the scanner cannot evaluate is its own verdict (continued)

- **`Verdict` is three states** — `Satisfied`, `Unsatisfied`, `Unevaluated` —
  and `all` / `any` / `not` fold between them. `may_hold` resolves `Unevaluated`
  once, at the one place that decides whether an item is test-only, and it
  resolves it towards keeping the item: an item in scope is a candidate for a
  finding, and an item dropped is a clean report for source nobody read.
- **`Hit`'s fields are private** with `rule()`, `line()` and `snippet()`. A
  consumer now borrows the evidence it was shown instead of holding a `String`
  it could edit into a different finding.
- **An empty test-scope stack is no scope**, stated as `matches!` on the stack's
  top rather than a `false` substituted for a missing entry.
- **`enter_function` keeps "no enclosing function" as `Option<usize>`** instead
  of flattening it to line `0` and re-expanding it on exit — a sentinel a walk
  could mistake for a real line.

Verified against planted controls on the shipped binary: a bare `.ok()` swallow
in production is reported; the same line under `#[cfg(test)]` is clean; under
`#[cfg(feature = "never")]`, `#[cfg(not(test))]` and `#[cfg(any(test, …))]` it
is reported, which is the fail-open-for-findings direction. `lgwks-deps scan` on
this repository: 383 files, zero findings. 176 lib tests and 51 scan tests pass.
### lgwks_deps — the resolved graph, the vendor report and the CLI's target resolution

The readers that build the gate's inputs each answered a value it did not have,
and the CLI each resolved "no path given" on its own.

- **A `[[package]]` block with no `version` is refused**
  (`LockError::VersionlessPackage`) instead of being read with an empty version.
  A resolved package is an identity of name *and* version, and an empty version
  is a value no line declared — `freshness` compared it with the registry's
  latest and the audit compared it with the approval's pin, so both decided on
  it. This is the same refusal `NamelessPackage` already was, for the other half
  of the identity. Cargo writes `version` for every locked package, so only a
  hand-edited or truncated lockfile reaches it.
- **`lock::Resolved`, `vendor::Missing` and `vendor::Report` are read through
  accessors.** `#[non_exhaustive]` blocks construction outside the crate, not
  mutation of a value a caller holds; these three are the readers' own output.
  `Report::missing()` returns a slice and `Report::is_complete()` is the verdict,
  so a caller can no longer empty the list that decides the exit code.
- **One `subject_path` resolves every command's target.** Six call sites each
  substituted `.` for an absent path; a command given no path audits the
  repository the operator is standing in, and that is now one named fact.
- **A verdict that arrives without a repository is refused.** `check` printed its
  receipt against `.` when the collection reached a verdict without a root,
  which is the exact defect `check_cli` pins — a receipt for a repository nobody
  asked about.
- **No command and an unknown command are distinct refusals**, each with its own
  message and the usage block, instead of an empty command name.
- **The debugger surface reads the `lgwks_std` manifest's own vocabulary**: an
  absent `default`/`trace` assignment is `None` and answers "does this list
  include that entry" for itself, rather than being read as an empty list.
- **A registry that publishes no version is `None`**, not an empty string, so
  `stale` is decided only between two versions crates.io and the lock both named.
- **`serde_json_number` keeps counts; `schema_number` renders the register's
  `u32` version.** The version went through a widening conversion whose failure
  was reported as `usize::MAX` — a register version this binary could not name.
- **`scan`'s excluded-directory test reads the directory's own name.** A name
  that is not UTF-8 was read as empty, and an empty name matches no excluded
  directory only by inventing a name the filesystem does not carry.

### lgwks_deps — the seeded generators draw through one total generator

The simulation suites substituted a value for every draw they could not make: a
zero table length became `1`, a failed `u64`/`usize` conversion became index `0`,
and a zero modulus became `0`. Each of those answered a case with a number the
seed never chose, and one of them (`.max(1)` on an empty table) turned "no
elements to draw" into an out-of-range read rather than a refusal.

- **`Rng::pick` refuses an empty table** with `Option`, and **`Rng::pick_named`**
  refuses it with a typed `EmptyTable` naming the table that was empty. The draw
  is consumed before the table is inspected, so a seed's sequence no longer
  depends on how long the table it drew from was.
- **`sim_metadata_dimensions` uses the shared generator** instead of its own
  splitmix64 copy: two index/draw implementations in one test binary is the
  duplication the sweep exists to remove, and the suite's model — not the
  generator — is what its assertions are about.
- **Every generator in that suite is fallible** (`Outcome<T>`), so a draw the
  fixture could not make reaches the test that owns the seed as that draw's own
  refusal instead of as a value the generator invented.
- **A truncation cut is a drawn eighth of the document**, not a drawn raw
  offset: the family no longer has to convert a bound that a short document
  could reduce to zero, and every cut is a strict prefix by construction.
- **The register's `target` spelling is one named conversion** (`declared_scope`):
  Cargo's absent target and the register's `target = ""` are two spellings of
  one declaration, and the conversion between the two vocabularies was being
  written out at five call sites.
- Draws use `checked_rem`/`checked_div` rather than a masked modulus, which is
  what `clippy::arithmetic_side_effects` and `clippy::integer_division` require;
  the `max(1)` that made the old code read as total was the opposite.

Behaviour is unchanged for every reachable case: every table the suites draw from
is a non-empty constant or a slice whose emptiness the caller already tested.
The new arms are what an emptied fixture table *means*, and each is reachable
only by editing a fixture.
### lgwks_std — the test sweep's own fixtures, held to the same contract (9-axis sweep)

Every rust-guard finding in `crates/lgwks-std/tests/` is fixed at its cause. No
library behaviour changed; the traces every seeded family folds differ, because
the byte stream they draw from does.
