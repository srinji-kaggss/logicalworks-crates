# Consumer guides

Five Rust crates ship from this repository. Each is usable on its own, and each
has its own version. Pick the one whose problem you have, then read its guide
for the version you are installing.

## Choose a crate

| You need | Crate | Guide |
|---|---|---|
| Orchestration with bounded fan-out, deadlines, retries under a shared budget and tenant-keyed steps, written as `script!` | `lgwks_bot` | [lgwks-bot](lgwks-bot/index.md) |
| The proc-macro behind `script!`, used through `lgwks_bot` | `lgwks_macros` | [docs.rs](https://docs.rs/lgwks_macros) |
| Hex, base64, timestamps, UUIDs, hashing, globs, LEB128, retry budgets, a single-threaded executor, and default structured debugging | `lgwks_std` | [lgwks-std](lgwks-std/index.md) |
| One AST type across several languages, with bounded traversal, and the reader of the `script!` language | `lgwks_ast` | [lgwks-ast](lgwks-ast/index.md) |
| A place to opt into a third-party stack, plus commands that prove no unowned dependency or missing debugger wiring entered a build | `lgwks_deps` | [lgwks-deps](lgwks-deps/index.md) |

`lgwks_bot` depends on `lgwks_macros`, `lgwks_std`, `lgwks_ast` and
`lgwks_deps`; `lgwks_macros` depends on `lgwks_ast`, `lgwks_deps` and
`lgwks_std`; `lgwks_ast` depends on `lgwks_deps` through its `script` feature;
and `lgwks_deps` depends on `lgwks_std`. Adding one does not require adopting
the others. In particular, adding `lgwks_ast` to a project that already uses
tokio and serde_json does not ask you to replace them; the dependency policy in
[`../../AGENTS.md`](../../AGENTS.md) governs contributions to this repository,
not your application's manifest.

## Versions and the supported interface

The versions in each crate's manifest are the source of truth for this tree;
the registry is the source of truth for what you install. The release process in
[`../releasing.md`](../releasing.md) keeps the two apart: a tag is created only
after the upload it names.

| Crate | `Cargo.toml` version | Read it at |
|---|---|---|
| `lgwks_std` | 3.0.2 | [docs.rs/lgwks_std](https://docs.rs/lgwks_std) |
| `lgwks_bot` | 3.1.1 | [docs.rs/lgwks_bot](https://docs.rs/lgwks_bot) |
| `lgwks_ast` | 2.2.0 | [docs.rs/lgwks_ast](https://docs.rs/lgwks_ast) |
| `lgwks_deps` | 4.2.0 | [docs.rs/lgwks_deps](https://docs.rs/lgwks_deps) |
| `lgwks_macros` | 1.1.4 | [docs.rs/lgwks_macros](https://docs.rs/lgwks_macros) |

From those versions on, three surfaces carry a compatibility promise. The
orchestration surface is `script!`, `try_join!` and the `script` module of
`lgwks_bot`, plus `lgwks_ast::script`. The code-understanding surface is
`lgwks_ast`'s documented parser (`Language`, `try_parse`, `inspect_ast`,
`ParseError`, `Diagnostic`) and scanner (`lgwks_ast::scan`). The admission
surface is `lgwks_deps`'s gate (`check_dependencies`, `check_verdict`,
`Verdict`, `Refusal`, `GateError`, `contract::Contract`) and its
`storefront` module. Every other item is `#[doc(hidden)]` support code with no
compatibility promise (#526). The migration notes are under
[`../changelog/`](../changelog).

Many guide pages below describe that support code in depth. They remain
accurate about the tree they cite, and they are reference for contributors and
for readers who accept that those paths can change in any release. Pages that
cover only part of the history say so at the top:

- [Guidance sessions](lgwks-bot/sessions.md)
- [Resolution and degraded verdicts](lgwks-bot/resolution.md)
- [The registry](lgwks-bot/domains.md)

## How these pages cite

Each factual claim names the file and line it came from, in the form
`path/to/file.rs:NNN`, and the `check-doc-citations` binary of `lgwks_deps`
verifies that every one resolves. Line numbers are from the head of this branch.
Where a claim could not be established from the inspected source,
the page says so rather than describing the intent.
