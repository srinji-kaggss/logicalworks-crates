# lgwks_std 3.0.3 · lgwks_bot 3.1.2 · lgwks_ast 2.2.1 · lgwks_deps 4.2.1 · lgwks_macros 1.1.5

2026-10-21

Five patch releases. No public item changed shape: every crate is republished so
that crates.io and docs.rs serve the README, the decision page and the crate
documentation as they now read. One binary and one test module are the only new
code, and neither is part of a library's surface.

## All five crates

- The README now opens by naming the publisher and all five crates in one
  sentence, and links `docs/should-i-use.md`.
- `docs/should-i-use.md` answers the question a reader arrives with, including
  where each crate is the wrong choice. It is the page an assistant can quote.

## lgwks_deps 4.2.1

- `render-site` writes the project page, `robots.txt` and `sitemap.xml` into a
  directory. It is a binary, not a public item; the repository holds Rust only,
  so the page is generated rather than checked in.
- `sim_estate` sweeps the closed estate list (#701) over seeded name draws: a
  published name is admitted, and another case, an extension, a bare prefix or
  an arbitrary word is refused. 61 tests in twelve bands of 64 seeds, under a
  second, and the same seed replays the same names.
- `lgwks_deps::estate` gained no public item: the recognition is still the
  `pub(crate)` predicate the audit reads.

## lgwks_std 3.0.3 · lgwks_bot 3.1.2 · lgwks_ast 2.2.1 · lgwks_macros 1.1.5

- Documentation only: the crate pages, the README text and the links between
  them. No item, feature or behaviour moved.

## Verifying the release

```sh
cargo run -p lgwks_deps --bin published-consumer
```

It builds every published crate from crates.io at its newest tag and exits
non-zero when one fails or a tag names a version the registry does not serve.
The release is done at `published consumers: all passed`.
