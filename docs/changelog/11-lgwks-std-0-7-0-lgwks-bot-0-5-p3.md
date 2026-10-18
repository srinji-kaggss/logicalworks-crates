# lgwks_std 0.7.0 / lgwks_bot 0.5.0 / lgwks_deps 0.1.13 - 2026-09-28 (3/5)

Release: lgwks_std 0.7.0 / lgwks_bot 0.5.0 / lgwks_deps 0.1.13 - 2026-09-28. Page 3 of 5. [Changelog index](../../CHANGELOG.md)

### lgwks_bot Fixed (continued)

- `process` now enables `time`. `ProcessSpec`'s deadline is enforced with
  `rt::time::timeout`, and `rt::time` is behind the `time` feature, so a
  `--no-default-features --features process` build did not compile
  (issue #131). The CI feature matrix now has a `Clippy process-only` lane
  so that combination cannot rot again.
