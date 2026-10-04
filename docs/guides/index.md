# Consumer guides

Four Rust crates ship from this repository. Each is usable on its own, and each
has its own version. Pick the one whose problem you have, then read its guide
for the version you are installing.

## Choose a crate

| You need | Crate | Guide |
|---|---|---|
| Hex, base64, timestamps, UUIDs, hashing, globs, LEB128, retry budgets, a single-threaded executor, and default structured debugging | `lgwks_std` | [lgwks-std](lgwks-std/index.md) |
| An effect that runs when an observed value changes, behind a capability check | `lgwks_bot` | [lgwks-bot](lgwks-bot/index.md) |
| One AST type across several languages, with bounded traversal | `lgwks_ast` | [lgwks-ast](lgwks-ast/index.md) |
| A place to opt into a third-party stack, plus commands that prove no unowned dependency or missing debugger wiring entered a build | `lgwks_deps` | [lgwks-deps](lgwks-deps/index.md) |

The crates share a release process, not a dependency graph. `lgwks_bot` depends
on `lgwks_std`, `lgwks_deps` depends on `lgwks_std`, and `lgwks_ast` depends on
neither. Adding one does not require adopting the others. In particular, adding
`lgwks_ast` to a project that already uses tokio and serde_json does not ask you
to replace them; the dependency policy in
[`../../AGENTS.md`](../../AGENTS.md) governs contributions to this repository,
not your application's manifest.

## Stable and development status

A crate's version in its manifest and the symbols actually present at a git tag
are two different observations. The table below records what was checked, not
what is implied.

| Crate | `Cargo.toml` version | Newest tag | Checked |
|---|---|---|---|
| `lgwks_std` | 1.0.0 | `lgwks_std-v0.10.0` | Manifest raised to 1.0.0; 0.10.0 is on crates.io; no 1.0.0 tag until upload |
| `lgwks_bot` | 1.0.0 | `lgwks_bot-v0.8.0` | Manifest raised to 1.0.0; 0.8.0 is on crates.io and exports `script` |
| `lgwks_ast` | 1.0.0 | `lgwks_ast-v0.4.0` | Manifest raised to 1.0.0; 0.4.0 is on crates.io |
| `lgwks_deps` | 1.0.0 | `lgwks_deps-v0.4.0` | Manifest raised to 1.0.0; 0.4.0 is on crates.io |
| `lgwks_macros` | 1.0.0 | `lgwks_macros-v0.1.2` | Manifest raised to 1.0.0; 0.1.2 is on crates.io |

Those are source tags, and a source tag is not a registry upload.
`docs/releasing.md` keeps the two apart and warns that a release created before
the upload is a claim the registry does not support. Confirm the version you are
installing against docs.rs or `cargo tree` in your own project before relying on
a symbol.

### `lgwks_bot` modules and their first release

`lgwks_bot-v0.5.0` was the first tag to export `broker`, `cap`, `domain`,
`effect`, `error`, `frontier`, `gate`, `interface`, `journal`, `json`,
`language`, `retry`, `rt`, `semantic`, `session`, `spec` and `verb`
(`git show lgwks_bot-v0.5.0:crates/lgwks-bot/src/lib.rs`). `lgwks_bot-v0.6.0`
added `script` (the `script!` macro, feature `script`), which needs
`lgwks_macros`.

The pages in this tree that document the newer surface are marked at the top:

- [Guidance sessions](lgwks-bot/sessions.md)
- [Resolution and degraded verdicts](lgwks-bot/resolution.md)
- [The registry](lgwks-bot/domains.md)

Those pages need `lgwks_bot` 0.5.0 or later. Everything else under `lgwks-bot/`
describes symbols present in the 0.4.2 tag as well.

## How these pages cite

Each factual claim names the file and line it came from, in the form
`path/to/file.rs:NNN`, and `scripts/check-doc-citations.py` verifies that every
one resolves. Line numbers are from the head of this branch.
Where a claim could not be established from the inspected source,
the page says so rather than describing the intent.
