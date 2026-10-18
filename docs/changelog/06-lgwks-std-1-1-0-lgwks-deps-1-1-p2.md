# lgwks_std 1.1.0 / lgwks_deps 1.1.0 / lgwks_macros 1.1.0 / lgwks_bot 1.1.0 - 2026-10-05 (2/2)

Release: lgwks_std 1.1.0 / lgwks_deps 1.1.0 / lgwks_macros 1.1.0 / lgwks_bot 1.1.0 - 2026-10-05. Page 2 of 2. [Changelog index](../../CHANGELOG.md)

### lgwks_macros — refusals by path, and lints the consumer enforces (#265) (continued)

- `script!` now refuses `unwrap`/`expect`/`unwrap_err`/`expect_err` called by
  path (`Option::unwrap(x)`) as well as by method; every `assert*!` and
  `debug_assert*!`; `process::exit`, `process::abort` and `mem::forget`;
  indexing and slicing; a `use` inside a flow that renames any refused call;
  and a machine path anywhere in a string literal. Each refusal names its
  replacement. A script that relied on any of these no longer compiles.
- Every generated flow carries `#[forbid(..)]` on `unsafe_code` and on the
  clippy lints for the same defects, after the author's own attributes, so a
  name imported outside the script is refused by the consumer's `cargo clippy`
  and an `#[allow]` above a flow cannot lower it.
