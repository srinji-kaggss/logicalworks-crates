# lgwks_std 3.0.0 / lgwks_bot 3.0.0 / lgwks_ast 2.0.0 / lgwks_deps 4.0.0 / lgwks_macros 1.1.2 - 2026-10-18 (1/2)

Release: lgwks_std 3.0.0 / lgwks_bot 3.0.0 / lgwks_ast 2.0.0 / lgwks_deps 4.0.0 / lgwks_macros 1.1.2 - 2026-10-18. Page 1 of 2. [Changelog index](../../CHANGELOG.md)

### Upgrading

Four crates take a major bump and `lgwks_macros` a patch. No item is removed:
every path a 2.x caller wrote still resolves, so a build that compiled before
still compiles. What moves is the supported interface (#526): `lgwks_bot::script!`
is it, and every other public path is outside what the crates support. A path
rustdoc no longer lists is `#[doc(hidden)]`; it carries no compatibility promise
and may change in any later release. Write orchestration as a `script!` flow.
For a capability the lexicon has no word for, the guide page that documents the
item names the test that runs its example.

- **lgwks_bot 3.0.0** — the crate root lists `script!`, `try_join!` and the
  `script` module; every other root module and re-export, and the `domains!`
  macro, is `#[doc(hidden)]` (`cargo doc` pages 1,086 → 106). The README is seven
  pages whose examples run as tests (`tests/it/readme_front_door.rs`,
  `tests/it/rt_front_door.rs`); the root README's quick start is a `script!` flow.
- **lgwks_std 3.0.0** — the root lists only what the `script!` expansion reaches
  (`cargo doc` pages 317 → 2).
- **lgwks_ast 2.0.0** — the root lists `script` only (172 → 3 pages); `error`,
  `diagnostic`, the parser re-exports and `scan` are hidden. The README and
  `parser.md` run as doctests on one hidden item.
- **lgwks_deps 4.0.0** — the root lists only what `script!` reaches (76 → 2).
- **lgwks_macros 1.1.2** — no public change; its `lgwks_deps` requirement moves
  to 4.

The surface only shrinks from here: the `std-first` lane refuses a library file
that declares more `pub` items than `contract/PUBLIC_SURFACE.toml` records
(INV-SURFACE-1) and a documentation file that holds more direct-use examples than
its `[examples]` row (INV-EXAMPLE-1).
