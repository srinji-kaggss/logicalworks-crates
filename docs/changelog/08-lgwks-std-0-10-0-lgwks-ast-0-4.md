# lgwks_std 0.10.0 / lgwks_ast 0.4.0 / lgwks_deps 0.4.0 / lgwks_bot 0.8.0 / lgwks_macros 0.1.2 - 2026-09-30

Release: lgwks_std 0.10.0 / lgwks_ast 0.4.0 / lgwks_deps 0.4.0 / lgwks_bot 0.8.0 / lgwks_macros 0.1.2 - 2026-09-30. Page 1 of 1. [Changelog index](../../CHANGELOG.md)

The third cut of the day, for the #191-#194 review fixes that landed in #199.
`lgwks_std`, `lgwks_deps`, `lgwks_ast` and `lgwks_bot` each carry a break, so
each takes the minor position. `lgwks_macros` only raises its `lgwks_deps`
requirement and takes a patch.

### lgwks_std Breaking

- `fs::capability::Dir::entry_names` takes `ListLimits` and returns a
  `Listing` instead of `Vec<OsString>`. Names are `String`s, the only name type
  the other `Dir` methods accept; a name that is not UTF-8 is counted by
  `Listing::unaddressable` rather than returned. The listing is bounded by entry
  count and name bytes, is sorted, and reports `is_truncated`. (#192)

### lgwks_std Added

- `http::Options::deadline` bounds the whole call, from the first lookup to the
  last body byte, across every redirect hop. `timeout` still bounds each phase
  of each hop. An expired deadline fails at `FailureStage::Deadline`. (#191)

### lgwks_std Changed

- A response body's buffer grows geometrically up to `max_body_bytes` instead of
  reserving exactly each 8 KiB read. (#191)
- `WalkLimits` states which part of a bounded walk depends on filesystem order:
  completeness does not (unless symlinks are followed without sorting); the
  entries an incomplete report keeps, in the directory where the budget ran
  out, do. (#192)

### lgwks_deps Breaking

- `metadata::read`, `metadata::workspace_members`, `check_dependencies`,
  `check_dependencies_against`, `check_invariants` and `invariants::check`
  return their result inside `metadata::Collected`. When Cargo's output was
  read in full but a capture file could not then be removed, the result is kept
  and the cleanup failure travels beside it (`Collected::into_parts`); before,
  the whole collection was refused. The CLI prints it as a `WARN` line on
  stderr and keeps the verdict. (#193)

### lgwks_deps Changed

- A `ProcessCleanup` refusal names the failed step (kill or reap), the cargo
  pid, the OS error, and each capture path it could not remove. Its
  `Error::source` is the primary failure when there is one. (#193)
- `check --json` may write one `WARN` line to stderr, for a Cargo capture file
  that could not be removed; stdout stays one JSON document. (#193)

### lgwks_ast Fixed

- A span whose offset falls inside a multi-byte character resolves to that
  character's start instead of panicking. (#194)

### lgwks_ast Breaking

- `AstMetrics` no longer implements `Default`. A default value had
  `complete == false` and read as a partial walk that nothing performed. (#194)

### lgwks_ast Changed

- `ParseError::to_diagnostic` places an `InvalidSyntax` refusal at its earliest
  retained recovery node instead of at the end of the file. (#194)

### lgwks_bot Breaking

- `domain::net::NetState::body` is a method; the field is crate-private, so a
  held value keeps the `BODY_PREVIEW` bound. A poll is bounded by a 10 s
  whole-call deadline, redirects included. (#191)

### lgwks_macros Changed

- Requires `lgwks_deps` 0.4. No change to the `script!` syntax.
