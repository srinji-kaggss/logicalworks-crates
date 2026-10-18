# lgwks_std 2.0.0 / lgwks_ast 1.1.0 / lgwks_deps 3.0.0 / lgwks_macros 1.1.1 / lgwks_bot 2.0.0 - 2026-10-06 (7/20)

Release: lgwks_std 2.0.0 / lgwks_ast 1.1.0 / lgwks_deps 3.0.0 / lgwks_macros 1.1.1 / lgwks_bot 2.0.0 - 2026-10-06. Page 7 of 20. [Changelog index](../../CHANGELOG.md)

### lgwks_std — the nine-axis sweep: the ceiling assertion asserts, so the scan gate sees no unlogged refusal (9-axis sweep) (continued)

`INV-SCAN-ZERO` refused this branch on the first CI run: the new
`assert_refused_at_small_ceiling` helper returned `Err`, and the repo's own
`lgwks-deps scan` detector (`unlogged-err-return`) only exempts a function the
harness calls through an attribute it recognises (`#[test]`, `#[test_case]`,
`#[rstest]`, or a `cfg` that requires `test`). A helper inside `mod tests` is
not exempt, and no real `#[test_*]` attribute exists to mark it.

The helper now asserts instead of returning. The only way it can fail is that
the answer was not `Error::BodyTooLarge { limit: SMALL_CEILING }`, and an
assertion carries that to the calling test with the answer in the message; a
`Result` here would have handed the caller an error to propagate before the test
could say what it saw. The three ceiling families keep their own framing and
their own wording.

```
$ cargo run --locked --release -p lgwks_deps --bin lgwks-deps -- scan
OK  scan clean — 384 files, zero findings
```

### lgwks_std — the nine-axis sweep: http names its spans, and every fixture thread is owned (9-axis sweep)

`http.rs` carried 19 findings: eight defaults, one suppression and ten repeated
blocks. All are repaired, and no request, refusal, timeout stage or body ceiling
changes.

- **URL validation and target redaction name their spans.** An authority with no
  `/`, `?` or `#` after it runs to the end of the remainder, which is what a URL
  ending at its authority looks like; redaction takes the authority, the suffix
  and the path as slices between delimiters it found, so userinfo, query and
  fragment are removed from spans that exist rather than spans that defaulted to
  empty.
- **The tests carry no suppression.** The `#[expect(clippy::disallowed_methods)]`
  over the test module is deleted: the three waits use
  `std::thread::park_timeout`, and the twelve fixture threads start through one
  `fixture_thread` helper that uses a *named* `thread::Builder` and hands back
  the handle the test already joins — the bare `std::thread::spawn` this
  workspace bans is gone from the crate.
- **One fixture, one assertion, three families.** `serve_held` is the silent and
  the replying server with the reply as the difference;
  `assert_refused_before_dialing` asserts each refused URL with its own text;
  `assert_refused_at_small_ceiling` asserts the ceiling's refusal once, so the
  chunked, close-delimited and non-UTF-8 families state only what their own
  framing adds. The fixture server parses `Content-Length` into an accumulator
  rather than out of an `Option`.

Tests: 47 pass, including `a_deadline_bounds_the_whole_redirect_chain`,
`a_deadline_bounds_a_trickled_body`, `body_timeout_preserves_stage_and_class`,
`eof_probe_timeout_preserves_stage_and_class` and the seeded `sim_http` families.

### lgwks_std — the nine-axis sweep: the hash suite hashes whole slices and whole frames (9-axis sweep)

`sim_hash.rs` built its expectations out of nine defaults. Each is repaired, and
every trace hash is unchanged: `the_same_seed_replays_to_the_same_hash_trace`
and the other 22 tests pass.

- A chunk of the message being walked is a slice, not a lookup that defaulted to
  empty — a zero-length chunk would have hashed fewer bytes than the reference.
- The three parts that must tile a message are compared as lengths, with no
  narrowing between them and the sum.
- The framed expectation builds the documented eight-byte length prefix from the
  length's own bytes, in a `frame_prefix` written here from the contract rather
  than taken from the crate, so the oracle stays independent.
- A 63-character prefix of a 64-character digest hex is a slice of that hex.
- Replacing one ASCII byte of ASCII hex keeps text, and the arm that says so
  names that invariant instead of substituting an empty string.
- The flipped bit is the draw itself, already below eight, so the mask is a
  shift of a byte by at most seven.

### lgwks_std — the nine-axis sweep: INV-CODEC-1 is one property with two facades (9-axis sweep)

`json` and `ron` are two codecs over one `serde` surface, so the invariant
INV-CODEC-1 states is one property, not two: an unescaped field is borrowed from
the input and an escaped one is not. The assertion that proves it was written
twice, once per codec, and the guard reads the two copies as the same block —
which is the point: two copies of one property drift.

- **The contract now lives in one test-only module.** Each codec contributes only
  its own document and its own two entry points
  (`serde_facade::json_borrow_contract`, `serde_facade::ron_borrow_contract`,
  and the matching escaped-field halves); the borrow assertion, the
  `Borrowed` fixture and the pointer-containment check are written once. Both
  test names INV-CODEC-1 cites are kept, and each is now three lines that name
  its codec's half.
- **The round-trip fixture is named once per codec** (`POINT`), and each
  malformed-document assertion carries its own fact, so the codec tests differ
  only where the codecs do.
- **`examples/digest_timing.rs` carries no sentinel.** The sample count is a
  `usize` throughout, so neither the schedule's doubled total nor the warm-up
  comparison needs a narrowing conversion; the crop index is an exact euclid
  quotient; a batch too slow to record in a `u32` is a preemption and is dropped
  rather than recorded as a maximum, which would have set the crop and kept
  itself; and a run shorter than the crop keeps every sample.

The dudect harness runs on the release build at 40,000 samples per class:
`digest_eq t = 0.005` (no leak detected) against the `early_exit control
t = 1962.447` (leak detected, as it must be). 67 codec, RON and JSON tests pass.

### lgwks_std — the nine-axis sweep: one poison recovery per lock shape, and a thread that is owned (9-axis sweep)
