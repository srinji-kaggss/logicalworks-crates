# lgwks_std 0.9.0 / lgwks_deps 0.3.0 / lgwks_bot 0.7.0 / lgwks_macros 0.1.1 - 2026-09-30

Release: lgwks_std 0.9.0 / lgwks_deps 0.3.0 / lgwks_bot 0.7.0 / lgwks_macros 0.1.1 - 2026-09-30. Page 1 of 1. [Changelog index](../../CHANGELOG.md)

The second cut of the day, for the #157, #158, #159, #161, #162, #167 and #168
work that landed in #197. `lgwks_std` and `lgwks_deps` each carry a break, so
they take the minor position. `lgwks_bot` re-exports `lgwks_std::json` and
depends on `lgwks_deps`, so it moves with them. `lgwks_macros` only raises its
`lgwks_deps` requirement and takes a patch. `lgwks_ast` does not move.

### lgwks_std Breaking

- `pattern::PatternError` fields `pattern` and `message` are private; read them
  with `pattern()`, `message()` and the new `kind()` (`PatternErrorKind`:
  `PatternTooLarge`, `CompiledTooLarge`, `Syntax`, `Other`). It no longer
  implements `Clone`, and `Regex`'s `Debug` output quotes and escapes the
  pattern (`Regex("a+")`, not `Regex(a+)`). (#168)
- `ron::to_writer` returns `WriterError`, which separates `Serialize` (nothing
  was written) from `Write` (the writer may hold a prefix), instead of folding
  an I/O failure into `ron::Error`. (#162)

### lgwks_std Added

- `pattern::Regex::with_config(pattern, PatternConfig)` returns a
  `BoundedRegex` that enforces source-pattern bytes, compiled size, nesting,
  input bytes and replacement-output bytes. Every search, split and replace
  returns `PatternRunError` on a breach rather than a partial result.
  Bounded replacement expands `$name`, `${n}` and `$$` exactly as the engine
  does, including `${+1}`, and is tested against it template by template.
  `Regex::replace_borrowed` and `replace_all_borrowed` return `Cow` and do not
  allocate when nothing matched. The docs now state the real cost: one search
  is O(m·n), while full greedy iteration may be O(m·n²). (#168)
- `wire::format_descriptor()` reports the byte order, pointer width and `u32`
  alignment of the archives this build produces. The module no longer claims a
  canonical or versioned encoding. (#167)
- `json` and `ron` serializers accept unsized values (`T: ?Sized`), and
  `from_str`/`from_slice` take `Deserialize<'de>`, so borrowed fields such as
  `&str` deserialize without copying. `ron` re-exports `serde`. (#162)

### lgwks_deps Breaking

- `contract::Contract::entries`, every `contract::Entry` field,
  `invariants::Register::entries`, every `invariants::Entry` field and
  `invariants::Outcome::id` are crate-private. Use `Contract::entry_count`,
  `Register::entry_count` and `Outcome::id()`. A parsed approval can no longer
  be edited after it passed validation. `Contract::approval_for` and
  `approvals_for` still answer whether a crate is approved, but the `Entry`
  they return is opaque outside the crate. (#157)
- `ContractError::BadDate` and `invariants::ErrorKind::BadDate` carry the
  field's `line`. `ContractError` gains `InvalidString`, `DuplicateEntryKey`
  and `InvalidField`. (#157)

### lgwks_deps Fixed

- Both registers read one strict TOML subset. A repeated key in an entry is
  refused and names both lines instead of letting the last write win. Values
  are single-line basic strings; no value may contain a control character,
  raw or escaped, and literal, multiline, bare and trailing-token values are
  refused. `source`, `allowed_kinds`, `allowed_consumers` and the
  crate/owner identifiers are closed vocabularies. `approved_on` must be a
  real Gregorian date, so `2026-13-45` and `1900-02-29` are refused. (#157)
- Workspace metadata in which a member has no package record, several records
  or a shared manifest directory is refused as a schema error instead of
  being filtered into an empty or misattributed graph. (#158)
- Metadata collection is tested with an injected fault at every stage
  (capture, spawn, wait, stat, read, kill, reap, unlink). Each stage either
  removes its capture files or leaves them in a retryable `CleanupObligation`.
  (#159)

### lgwks_bot Changed

- Depends on `lgwks_std 0.9` and `lgwks_deps 0.3`. `lgwks_bot::json` is
  `lgwks_std::json`, so its serializers now accept unsized values and its
  decoders borrow from their input, as described above.

### lgwks_macros Changed

- Depends on `lgwks_deps 0.3`. The `script!` syntax is unchanged.
