# lgwks_std 2.0.0 / lgwks_ast 1.1.0 / lgwks_deps 3.0.0 / lgwks_macros 1.1.1 / lgwks_bot 2.0.0 - 2026-10-06 (11/20)

Release: lgwks_std 2.0.0 / lgwks_ast 1.1.0 / lgwks_deps 3.0.0 / lgwks_macros 1.1.1 / lgwks_bot 2.0.0 - 2026-10-06. Page 11 of 20. [Changelog index](../../CHANGELOG.md)

### lgwks_deps — the policy refusals report what the register said (continued)

- **`declared_scope` is published.** Cargo spells an unconditional dependency
  with no `target` key; the register spells the same declaration `target = ""`
  (INV-DEP-13). The gate compared the two vocabularies through an inline
  `unwrap_or("")` in two places, and the simulation suites carried their own
  copy of the same rule. It is one public function now, with the rule in one doc
  and a doctest that states both spellings.
- **A drift refusal carries the bit the entry authored.** `DefaultFeaturesDrift`
  and `OptionalityDrift` reported `approved` as a plain `bool` the gate filled with
  `unwrap_or(true)` / `unwrap_or(false)`. The refusal only exists for an *authored*
  dimension, so the invented bit was never the answer — it is `Option<bool>` now,
  and the message prints the bit the register carries.
- **A target refusal names both sides in the register's vocabulary.** An authored
  `target = ""` and an absent `target` key both print `<none>`, through one named
  `scope_label`.
- **An unconstrained feature dimension prints that it admits no set**, rather than
  an empty set that reads as a set with no members.
- **The two manifest checks share one reader.** `dependency_declarations` and
  `declared_name` replace two copies of the same six-line walk, so a table header
  cannot be read one way in one manifest and the other way in the other.
- **The `vendor.rs` test fixture's root name drops the process id.** `check-std-first`
  refused it (a pid is reused by the OS, so it names no run) and the exemption that
  had covered it is deleted rather than re-anchored. The discriminator is
  nanoseconds plus the `NEXT` atomic, which is what that comment had already
  concluded: the atomic separates two tests on two threads, the nanoseconds
  separate two runs. `check-std-first`'s exemption list is one entry shorter.

New regression test: `seeded_scope_pairs_render_both_spellings_of_an_unconditional_edge`
(`tests/it/sim_policy_properties.rs`) drives the public API over every scope pairing
the register grammar admits, one seed for both draws, and asserts the rendered
refusal text — a refusal printing `""` for an unconditional edge would pass every
verdict assertion and be unreadable to the operator who has to act on it.

`python3 scripts/check-std-first.py`: std-first holds, 357 source files.
`python3 scripts/check-doc-citations.py`: 100 pinned lines across 13 pages, all
unchanged. `cargo clippy -p lgwks_deps --all-targets` passes on both
`--all-features` and `--no-default-features`. 303 crate tests pass.
### lgwks_deps — the metadata collection answers for what it could not read

- **`poll_quantum` parks instead of sleeping.** The `#[expect(clippy::disallowed_methods)]`
  on `std::thread::sleep` named a substitution where the ban already names a
  replacement: `std::thread::park_timeout`, which is the bounded synchronous
  wait in a crate with no reactor to block.
- **A dependency the `--no-deps` read never resolved keeps no licence record.**
  `direct_edges` wrote `DeclaredLicense { license: None, license_file: None }` for
  a package that read did not see, which is indistinguishable from a package
  that declares no licence — the one thing the licence audit (#208) exists to
  tell apart. The fields stay `None` and the audit refuses an edge whose terms
  are unknown.
- **`Instant::checked_add` failing is not a deadline that expired.** The old
  `unwrap_or_else(Instant::now)` refused every child immediately over a timeout
  of a few centuries; an unrepresentable deadline is now `None`, which says the
  clock's range ended and the byte budget is what bounds the child.
- **`MAX_CAPTURE_BYTES` is one named bound** for the two `u64::try_from(stream_cap)`
  sites, and `Resolve` names the two feature selections so the `cargo metadata`
  argument list is written once. A copy of that list is where a flag goes
  missing.
- **A pre-epoch clock is refused on the wasm capture path** rather than floored to
  the epoch, which named every capture of the process the same instant.
- **The stdout and stderr flood families assert through one check**, so the two
  cannot drift into testing different budgets.

Verified end to end: `lgwks-deps check .` reports 33 approvals, 45 edges, 0
refusals, exit 0, with the same contract and subject digests as before;
`lgwks-deps invariants .` resolves 4 invariants; `lgwks-deps scan` reports 383
files and zero findings. 176 lib tests pass, including the real-process deadline,
descendant-capture and flood families.
### lgwks_deps — the invariant register answers for what it did not read

`build` read `id` through `.map_or_else(|| "<unnamed>".to_owned(), …)`, so every
refusal about a block that declared no identifier named an invariant no register
ever wrote. `ErrorKind::MissingField` now carries the block's own line and an
`Option<String>` id, and the message is `line N: invariant block is missing
required field "id"`.

Five more readers each resolved an absent value to a stand-in:

- **`lint_table` names both spellings.** `[workspace.lints.clippy]` and
  `[lints.clippy]` declare one namespace — the second inherited by every member —
  and the equivalence is now written as two arms rather than as a default.
- **`lint_level` states the terminator's two cases**: the level ends at the first
  `,` or `}`, or runs to the end of the tail when the writer closed the table on
  the same line.
- **`check_scope` uses `split_once`**, so a scope with no `::` is a crate name
  carrying no module path, by the type rather than by a substituted first
  segment.
- **`strip_item_prefix` names the two `extern` shapes**: `extern "C" fn f()` names
  an ABI and `extern fn f()` does not, and both are stepped past deliberately.
- **`split_attribute` splits instead of defaulting.** Both cuts were
  `body.get(..).unwrap_or("")`; `char_indices` yields only boundary offsets and
  `]` is one byte, so both cuts are on a boundary by construction.

The required-field loop is what proves `approved_on` and `id` are present before
the refusals that name them, and each is a refusal rather than a substitution if
that ever stops holding.

Verified end to end: `lgwks-deps invariants .` on this repository resolves 4
invariants; a truncated register reports `line 1: invariant block is missing
required field "id"`, and a named block reports its own line. 176 lib tests and
44 register tests pass.
### lgwks_deps — a `cfg` the scanner cannot evaluate is its own verdict

The `test`-scope detectors decided "is this item test-only?" by evaluating its
`cfg` expression under every assignment of its free atoms. An atom the
assignment had not recorded was read as `true`, which made the expression easier
to satisfy and so kept the item in scope — the safe direction, and a value the
expression never had. `unwrap_or(true)` hid that from every caller above it.
