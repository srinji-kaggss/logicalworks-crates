# Consumer guides

Four Rust crates ship from this repository. Each is usable on its own, and each
has its own version. Pick the one whose problem you have, then read its guide
for the version you are installing.

## Choose a crate

| You need | Crate | Guide |
|---|---|---|
| Hex, base64, timestamps, UUIDs, hashing, globs, LEB128, retry budgets, a single-threaded executor | `lgwks_std` | [lgwks-std](lgwks-std/index.md) |
| An effect that runs when an observed value changes, behind a capability check | `lgwks_bot` | [lgwks-bot](lgwks-bot/index.md) |
| One AST type across several languages, with bounded traversal | `lgwks_ast` | [lgwks-ast](lgwks-ast/index.md) |
| A place to opt into a third-party stack, and a command that proves no unowned dependency entered a build | `lgwks_deps` | [lgwks-deps](lgwks-deps/index.md) |

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
| `lgwks_std` | 0.6.7 | `lgwks_std-v0.6.6` | Manifest raised in the 2026-09-21 cut; no tag for it yet |
| `lgwks_bot` | 0.5.0 | `lgwks_bot-v0.4.2` | Manifest raised in the 2026-09-21 cut; no tag for it yet, and the 0.4.2 tag's surface differs from `main` (below) |
| `lgwks_ast` | 0.2.2 | `lgwks_ast-v0.2.2` | Tag exists in this repository |
| `lgwks_deps` | 0.1.13 | `lgwks_deps-v0.1.12` | Manifest raised in the 2026-09-21 cut; no tag for it yet |

Those are source tags, and a source tag is not a registry upload.
`docs/releasing.md` keeps the two apart and warns that a release created before
the upload is a claim the registry does not support. Confirm the version you are
installing against docs.rs or `cargo tree` in your own project before relying on
a symbol.

### `lgwks_bot` is the one crate whose `main` runs ahead of its newest tag

The newest `lgwks_bot` tag is `lgwks_bot-v0.4.2`, and its `lib.rs` exports
`cap`, `domain`, `error`, `gate`, `json`, `rt`, `spec`, and `verb`. `main`
exports those plus `broker`, `effect`, `frontier`, `interface`, `journal`,
`language`, `retry`, `semantic`, and `session`. If you install
`lgwks_bot = "0.4.2"`, none of those nine is there.

The manifest reads `0.5.0` because the release commit for the 2026-09-21 cut has
landed, but no `lgwks_bot-v0.5.0` tag exists and nothing has been uploaded, so
`0.5.0` is not installable either. Until the tag exists, `main` is the only place
those nine modules can be read.

The pages in this tree that document the newer surface are marked at the top:

- [Guidance sessions](lgwks-bot/sessions.md)
- [Resolution and degraded verdicts](lgwks-bot/resolution.md)
- [The registry](lgwks-bot/domains.md)

Everything else under `lgwks-bot/` describes symbols present in the 0.4.2 tag.

## How these pages cite

Each factual claim names the file and line it came from, in the form
`path/to/file.rs:NNN`, and `scripts/check-doc-citations.py` verifies that every
one resolves. Line numbers are from the head of this branch.
Where a claim could not be established from the inspected source,
the page says so rather than describing the intent.
