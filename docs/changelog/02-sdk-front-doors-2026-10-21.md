# lgwks_std 3.0.2 / lgwks_bot 3.1.1 / lgwks_ast 2.2.0 / lgwks_deps 4.2.0 / lgwks_macros 1.1.4 - 2026-10-21

Release: lgwks_std 3.0.2 / lgwks_bot 3.1.1 / lgwks_ast 2.2.0 / lgwks_deps 4.2.0 / lgwks_macros 1.1.4 - 2026-10-21.
[Changelog index](../../CHANGELOG.md)

### Upgrading

No item is removed or renamed, and no signature changes. `lgwks_ast` takes a
minor bump and `lgwks_deps` takes a minor bump because both gain names on their
supported surface; `lgwks_std`, `lgwks_bot` and `lgwks_macros` take patch bumps
and carry no code change at all. A consumer that compiled against 2.1.0 /
4.1.0 keeps compiling, and one that was reading a `pub` item through
`#[doc(hidden)]` now has a documented name and a promise.

The one behaviour a consumer can newly rely on is the shape of what the gate
returns: `check_verdict` hands back `metadata::Collected<Verdict>`, and
`into_parts` separates the verdict from the cleanup obligation that a refused
or interrupted metadata read could not confirm.

### lgwks_deps — the supported metadata surface (#526)

- **Added** `lgwks_deps::metadata::Collected` to the supported surface. It is
  the type `check_verdict` returns: the gate's verdict plus the cleanup
  obligation for the capture files a metadata read created,
  `Option<CleanupObligation>`. `Collected::into_parts` is the only way out of
  it, so a consumer cannot read the verdict and forget the cleanup.
- **Added** `lgwks_deps::metadata::MetadataError` to the supported surface. It
  is the typed reason a metadata read refused — the cargo command failed, the
  document would not decode, the byte budget was spent, the deadline passed —
  and it is what a consumer needs in order to `match` on a refusal instead of
  printing it.
- **Changed** the accepted public surface, exactly: `contract/PUBLIC_SURFACE.toml`
  now carries `"crates/lgwks-deps/src/metadata.rs" = 2`. The ceiling is exact
  per file, so the two added names are a visible change to the contract rather
  than a silent widening, and `check-std-first --surface-baseline` prints the
  same table the tree implies.
- **Added** the downstream proof, `crates/lgwks-deps/tests/it/sdk_surface.rs`.
  It names only public paths — `Contract`, `GateError`, `MetadataError`,
  `Refusal`, `Verdict`, `Collected<Verdict>` — and drives the real register
  through `check_verdict`, so a name that leaves the surface fails to compile
  in this repository instead of in a user's build. It also asserts that a
  complete metadata read leaves no cleanup obligation behind.

### lgwks_ast — 2.2.0

- The 2.1.0 surface is unchanged and stays documented: `Language`, `try_parse`,
  `inspect_ast`, `ParseError` and `Diagnostic` for the parser, `lgwks_ast::scan`
  for the zero-gate detectors, `lgwks_ast::script` for the script reader. This
  minor version is cut with the release that documents the admission surface it
  is used through; no parser behaviour changed.

### lgwks_std 3.0.2, lgwks_bot 3.1.1 and lgwks_macros 1.1.4

- Documentation and version only. No code, no public item, no expansion
  changed; the three patch bumps keep the five crates on one release line so
  the workspace requirements, every lockfile and the version tables state one
  set rather than two.

**Does not claim:** that an item outside the three documented surfaces carries
a compatibility promise (it does not; everything else stays `#[doc(hidden)]`);
that `Collected`'s cleanup obligation can be ignored (a consumer that drops it
leaks the capture files, which is why `into_parts` is the only accessor); or
that a metadata read is now cheaper — the change is visibility, and the
subprocess, byte budget and deadline are the ones 4.1.0 shipped.
