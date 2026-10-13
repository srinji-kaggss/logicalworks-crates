# lgwks_std notes and glob matching

Part of the [lgwks_std consumer guide](index.md).

## Notes that save a support round

**`online` is not part of `core`.** It is its own feature, with no external
dependency, and it does not come with the default build.

**There is no production temporary-directory path.** `tempfile` is a
`[dev-dependencies]` edge (`crates/lgwks-std/Cargo.toml`), so it is not in your
dependency graph.

**Typed errors derive through `lgwks_ast`, not here.** `lgwks_std` does not own
`thiserror`. A crate in this workspace that wants `#[derive(Error)]` names
`lgwks_ast` as `thiserror`:

```rust
extern crate lgwks_ast as thiserror;

#[derive(thiserror::Error, Debug)]
enum Diagnostic {
    #[error("parse refused: {0}")]
    Refused(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
```

The derive expands to absolute `::thiserror::__private<N>::...` paths resolved in
the consuming crate, so that `extern crate` line is what makes the expansion
resolve.

**`serde` derives need `#[serde(crate = "lgwks_std::json::serde")]`.** The JSON
facade re-exports serde, and the dependency policy forbids declaring `serde`
directly. The attribute points the derive expansion at the re-export. If you see
`cannot find serde in this scope`, add the attribute rather than running
`cargo add serde`.

### Glob matching

`glob::matches` is an anchored text matcher over caller-supplied paths. It is
case-sensitive, does not normalize Unicode, treats leading dots and backslashes
as ordinary characters, and counts Unicode scalar values rather than bytes or
grapheme clusters. `/` is never consumed by `?`, `*`, or a class (even a
negated one); only `**` crosses separators. Ranges sort by Unicode scalar
value. It does not accept native `OsStr`, normalize Windows separators, walk
directories, or implement full POSIX shell expansion.

The compatibility entry point keeps the former permissive dialect: an
unmatched `[` is literal and `**` may appear inside a component, as in
`a**b`. New callers can compile strictly and distinguish malformed patterns
from ordinary no-match results:

Classes use `[abc]`; an initial `!` or `^` negates the class, and an interior
`x-y` denotes an inclusive range ordered by Unicode scalar value. A hyphen at
either edge is literal. An initial `]` is a member when a later `]` closes the
class; without that closer, strict compilation returns an unclosed-class
error and the legacy dialect treats the opening `[` literally. Strict
compilation also rejects descending ranges. Backslash has no escape meaning;
put `*` or `?` in a class to match it literally. In strict syntax, `**` must
occupy a complete slash-delimited component. Legacy syntax keeps embedded
`**` during migration.

```rust
use lgwks_std::glob::{GlobPattern, GlobScratch};

let pattern = GlobPattern::compile("src/**/[a-z]?.rs")?;
let mut scratch = GlobScratch::new();
assert!(pattern.is_match_with("src/a1.rs", &mut scratch));
assert!(pattern.is_match_with("src/sub/a1.rs", &mut scratch));
# Ok::<(), lgwks_std::glob::PatternError>(())
```

`GlobPattern::compile` rejects unclosed classes, descending ranges, and `**`
that does not occupy a complete path component. The recognized component forms
include `**/`, `/**`, and `/**/`. Use
`GlobPattern::compile_with_dialect(pattern, GlobDialect::Legacy)` for a named
migration path. The compiled tokens and class ranges use O(M) storage; reused
scratch holds scalar indexing and two rolling rows in O(N), with no row
allocation per token. `glob` is not a drop-in replacement for the upstream
`glob::Pattern`: its accepted legacy forms differ, and it does not provide
directory traversal.

`crates/lgwks-std/README.md` carries the replacement matrix: which module stands
in for which third-party crate.
