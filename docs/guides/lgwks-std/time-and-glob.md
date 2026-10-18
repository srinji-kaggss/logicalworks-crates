# lgwks_std time and glob notes

Part of the [lgwks_std consumer guide](index.md). Moved from the crate README.

## Time profile and migration

`time::parse_rfc3339` and `time::format::to_rfc3339` use a zero-dependency,
nanosecond-precision `SystemTime` profile. Parsing validates numeric offset
hours (`00..=23`) and minutes (`00..=59`), refuses leap-second labels, and
returns the instant normalized to UTC. The original numeric offset and the
local-offset provenance of `-00:00` are not retained. Fractions longer than
nine digits are refused rather than truncated. Formatting is canonical RFC
3339 with a four-digit UTC year; an instant outside years `0000..=9999` returns
`FormatError::YearOutsideRfc3339`.

Clock conversion and formatting can now refuse values the platform cannot
represent. Handle `Result` from `time::format::{from_unix_parts, unix_parts,
to_rfc3339}` and `time::now_rfc3339`; match `UnixTimeError` or `FormatError` at
the boundary. Previous silent epoch fallback behavior is available only via
the deprecated, explicitly lossy `from_unix_parts_lossy` and
`unix_parts_lossy` functions. This is a source migration: callers that ignored
conversion failure must now handle it, and no checked API substitutes the Unix
epoch for an unrepresentable instant.

## Glob text semantics

`glob::matches(pattern, path)` remains the one-call legacy entry point. It is
anchored, case-sensitive, does not normalize Unicode, treats backslash and
leading dots as ordinary text, and interprets `?` and classes as one Unicode
scalar (not a grapheme cluster). `/` is excluded from `?`, `*`, and all classes,
including negated classes. `*` stays inside one slash-delimited segment;
`**` may cross separators. The legacy dialect accepts `**` inside a component
and treats an unmatched `[` as a literal. Classes use `[abc]`; a leading `!` or
`^` negates, and `x-y` is an inclusive Unicode-scalar range when the hyphen is
between two members. A hyphen at either edge is literal. A leading `]` is a
member only when a later `]` closes the class; `[]` is therefore unclosed.
Strict compilation rejects unclosed classes and descending ranges. Legacy
compilation treats an unclosed `[` as a literal and a descending range as
matching nothing. Backslash never escapes metacharacters; use a class such as
`[*]` or `[?]` to match those characters. There is no byte-matching policy.

New callers can use `GlobPattern::compile` for checked syntax. It reports an
unclosed class, a descending range, or `**` outside a complete path component
as `PatternError`. Strict `**` must occupy a complete slash-delimited
component, including `**/`, `/**`, and `/**/`. `GlobPattern::compile_with_dialect(...,
GlobDialect::Legacy)` is the named migration path when older permissive forms
must remain. A compiled pattern stores O(M) tokens and class ranges; a reusable
`GlobScratch` owns O(N) scalar indexing and two rolling rows, with no row
allocation per token. This is matching over caller-supplied text, not native
`OsStr` matching, path separator normalization, directory traversal, or full
POSIX shell behavior.

The upstream `glob::Pattern` is a useful Unix-shell matcher but rejects
unclosed classes and constrains `**`; its contract is not a drop-in replacement
for the legacy dialect. `globset` is optimized for compiled sets of filesystem
patterns, not this crate's single-pattern zero-dependency core. `lgwks_std`
keeps its own bounded automaton and names its differences instead of claiming
complete parity with either interface.

Compiling `src/**/[a-z]?.rs` once and matching it with one reused `GlobScratch`
accepts both `src/a1.rs` and `src/sub/a1.rs`; the integration test
`public_compile_reuse_and_legacy_call_share_text_semantics` in
`crates/lgwks-std/tests/it/glob_public.rs` runs that flow.

The scalar change is behaviorally breaking for callers that used multiple `?`
tokens to consume the UTF-8 bytes of one scalar. Update those patterns to the
intended scalar count; literal equality and all ASCII results stay the same.
