# The `lgwks_std` before/after measurement harness

This is the instrument that produced the before/after numbers for issues #153,
#154, #160 and #164, committed with its raw sample output in
[`results.txt`](results.txt). Like [`../README.md`](../README.md) it is a
measurement instrument rather than a consumer of the crate, and for the same
reason: it is its own Cargo workspace root, so the estate's dependency contract
(`contract/APPROVED.toml`, enforced by `lgwks-deps check`) never sees it and no
competitor or harness-only dependency can leak into the shipped graph.

## Running it

```sh
cd bench/std-measure
CARGO_TARGET_DIR=/tmp/lgwks-std-measure-target \
  cargo run --release -- results.txt
```

The single positional argument is where the full report is written; the same
text is printed to stdout. Omit it to print without writing. The run takes a
few minutes: 23 scenarios, 200 000 timed calls for each retry row and 20 000 for
each glob and similarity row, every call individually timed.

## What it measures, and the one rule it obeys

Every row is a raw per-call sample distribution over the **public** API of the
shipped crate: `GlobPattern::is_match_with`, `CheckedEvidence::verdict`,
`CheckedSimilarity::try_score`, and `RetryPolicy::delay`. Nothing reaches a
private helper or reimplements the measured code, because a benchmark of a copy
is a benchmark of the copy.

**The comparison rule.** To fill the `before` column of a row, check the
relevant fix out of history, re-run *this same binary* against that tree, and
take the numbers from its output. Two trees timed in two sessions are not
comparable — the numbers move with the machine, which is the same objection
`../README.md` raises and the same reason its "control pair" row exists. What
*is* portable across two runs is the **shape**, and the shape is asserted here
rather than left to the reader:

- #154 G1: p50 for `*a*` at n = 256, 512, 1024, 2048 roughly doubles per
  doubling of the path. The shape it replaced grew quadratically, so a run that
  cannot see linear growth means the tree is not the repaired one.
- #164 R1: `retry delay` at attempt 0 and at `u32::MAX` is flat. The
  `retry walk reference` rows beside them are the `O(attempt)` alternative the
  shipped shift-and-compare form replaced; they get slower with the index, which
  is what makes "flat" a claim about the right thing.

## The scenarios

| Rows | What they cover |
|---|---|
| `retry attempt {0,31,1000,u32::MAX}` | #164's acceptance attempt indices, at the 1 ns base and 30 s cap. Flat across four orders of magnitude. |
| `retry walk reference attempt {0,u32::MAX}` | The `O(attempt)` doubling-walk the shipped form replaced, timed identically. This is the comparison the flat rows are measured against. |
| `glob *a* n={256..2048}` | #154 G1/G2's exact counterexample, `"a".repeat(n)`, and the G2 table the issue names. |
| `glob *a**/b[0-9]? n={256..2048}` | The six-token pattern on the same input size, for the G2 table's neighbours. |
| `glob *a**/b[0-9]? path={256..2048} scalars` | The six-token pattern over directory paths, where the double-star transitions decide the cost. |
| `similarity checked composition` | The #160 S2 checked composition (cosine + edit + bounded set) as one timed region. |
| `jaccard unbounded n={100..4000}` | #160 S4's quadratic curve on the type that has no ceiling, which is the reason `BoundedJaccard` exists. |

## Reading a row

Each row reports `p50`, `p95` and `p99` as nearest-rank percentiles over the raw
per-call samples, plus the mean and a `trace`: an FNV-1a hash of the whole
sample vector. Two runs can share every percentile and still be different
measurements — the tail is what moves under load — and the hash is what makes
that difference visible in the committed file. The percentile is computed with
`sort_unstable` rather than any helper in `lgwks_std`, so this instrument and
the code it measures share no code.

The `jaccard unbounded` rows are the shape the #160 S4 comment reports, not a
like-for-like re-measurement of its harness: same quadratic curve, different
host and build, so the absolute values are not comparable to that comment's.
