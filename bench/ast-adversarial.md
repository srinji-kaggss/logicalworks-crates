# The `lgwks_ast` parse-budget rig: adversarial shapes, markdown and fan-out

Part of the [measurement rigs](README.md); throughput and peak RSS per grammar are
in [ast-budget.md](ast-budget.md).

## The adversarial shapes, and the finding behind them

Sorted by bare-parse p50. These are the rows where the crate's bounds earn their
keep, and the rows where they do not reach.

| grammar | shape | bytes | parse p50 | walk p50 | walk share | peak RSS |
|---|---|---:|---:|---:|---:|---:|
| `dart` | nested | 262 144 | **97.500 s** | 15.5 ms | 0.0% | 11 MiB |
| `scala` | longline | 501 490 | **29.812 s** | 10.3 ms | 0.0% | 75 MiB |
| `dart` | unbalanced | 131 072 | 24.897 s | 7.8 ms | 0.0% | — (killed at 120 s) |
| `ruby` | longline | 1 647 756 | 5.591 s | 59.0 ms | 1.0% | 692 MiB |
| `html` | nested | 262 141 | 4.484 s | 10.6 ms | 0.2% | 152 MiB |
| `haskell` | longline | 1 997 280 | 1.508 s | 60.1 ms | 3.8% | 310 MiB |
| `go` | longline | 1 880 190 | 1.075 s | 54.5 ms | 4.8% | 226 MiB |

**The parser, not the walk, is the unbounded work on hostile input.** On the three
worst rows the validation walk is 10–15 ms and the parse is 25–97 seconds: the
walk is three to four orders of magnitude cheaper. `dart` at 256 KiB of nested
braces is the extreme measured case — **97.5 seconds for a quarter of the byte
ceiling**, and at the ceiling it did not finish in 120 s. tree-sitter's GLR parser
is super-linear in nesting depth on some grammars. These are the **bare** parse
times, with no deadline. A checked parse (`try_parse`) now stops the parser at
`DEFAULT_PARSE_DEADLINE`, 10 s, and answers `ParseError::TimedOut`; a caller
names a tighter one through `try_parse_within`. The parser checks every hundred
operations, so the thread is released within the deadline plus microseconds
rather than after 97 s (INV-AST-5).

Two rows are recorded as killed rather than finished: `dart` `nested` and
`unbalanced` at 2 MiB, and `scala` `longline` at 2 MiB, all by the rig's own
120-second bound. `scala` `longline` is in the table at **512 KiB**, where it
completes in 29.8 s; the row in `ast-budget.tsv` carries
`note bytes=524288_not_2097152_unfinished_at_120s`. A rig that stopped at the
first of those would have reported 108 rows instead of 112, which is why the
timeout records the partial row rather than dropping it.

## Markdown is refused before the grammar sees it

`nested` and `unbalanced` on the markdown grammar are not slow: they are refused.

| shape | bytes | outcome | peak RSS |
|---|---:|---|---:|
| `representative` | 2 097 147 | accepted | 460 MiB |
| `longline` | 1 828 282 | accepted | 308 MiB |
| `nested` | 262 144 | `container-nesting-too-deep` | 4 MiB |
| `unbalanced` | 262 144 | `container-nesting-too-deep` | 4 MiB |

4 MiB against a 2.1 MiB floor, because nothing was parsed. Before
`MAX_MARKDOWN_CONTAINERS_PER_LINE` existed, those two rows *aborted the process*:
tree-sitter-markdown's external scanner serializes its open block containers into
a fixed 1 024-byte buffer and asserts when they do not fit, and every container
shape that overflows does so at 255 open containers. The full measurement — 17
shapes bisected from a child process, and the guard proved from 1 065 of them —
is in `INVARIANTS.md` under INV-AST-4.

## Bounded fan-out: 100, 1 000, 10 000, 100 000

`try_parse` of a 64 KiB source per parse, over a bounded fan-out. The level's
index range is split into one contiguous slice per worker at admission, so there
is no queue to overflow and no lock to hold, and `thread::scope` joins every
worker. Eight workers on fifteen cores; **the host's cap is fifteen, so eight was
chosen and named rather than assumed**.

| grammar | level | p50 | p99 | max | wall | throughput | peak RSS |
|---|---:|---:|---:|---:|---:|---:|---:|
| `rust` | 100 | 5 625 ns | 181 125 ns | 194 875 ns | 0.0004 s | 15 519 MB/s | 195 MiB |
| `rust` | 1 000 | 2 500 ns | 21 500 ns | 211 541 ns | 0.0009 s | 73 340 MB/s | 195 MiB |
| `rust` | 10 000 | 1 042 ns | 5 000 ns | 84 875 ns | 0.0030 s | 217 045 MB/s | 196 MiB |
| `rust` | 100 000 | 292 ns | 3 125 ns | 718 833 ns | 0.0122 s | 544 714 MB/s | 198 MiB |
| `dart` | 100 | 708 ns | 50 667 ns | 81 666 ns | 0.0004 s | 14 665 MB/s | 327 MiB |
| `dart` | 1 000 | 1 916 ns | 9 833 ns | 581 375 ns | 0.0009 s | 68 744 MB/s | 329 MiB |
| `dart` | 10 000 | 750 ns | 3 625 ns | 263 834 ns | 0.0030 s | 188 787 MB/s | 330 MiB |
| `dart` | 100 000 | 458 ns | 2 000 ns | 4 941 542 ns | 0.0332 s | 200 275 MB/s | 329 MiB |
| `javascript` | 100 | 2 458 ns | 186 833 ns | 189 750 ns | 0.0004 s | 17 495 MB/s | 258 MiB |
| `javascript` | 1 000 | 2 084 ns | 12 125 ns | 194 500 ns | 0.0008 s | 85 925 MB/s | 258 MiB |
| `javascript` | 10 000 | 1 167 ns | 4 333 ns | 182 541 ns | 0.0031 s | 209 961 MB/s | 257 MiB |
| `javascript` | 100 000 | 375 ns | 2 917 ns | 3 333 583 ns | 0.0119 s | 552 395 MB/s | 259 MiB |

All 112 tier runs exited 0 and refused nothing: 64 KiB of each grammar's own
source is inside every ceiling. **Peak RSS is flat in the level** — 195 to 198 MiB
across four orders of magnitude of concurrency for `rust`, because each worker
holds one tree at a time and the trees are 64 KiB sources. That is the number
worth having: the per-parse bound is what makes a fleet of them bounded.

The p99 falls as the level rises because a worker finishing its slice is retired
and the level's per-parse latency is measured over fewer contending threads, not
because the parse got cheaper: throughput rises by 35x from 100 to 100 000 for
`rust`, which is the eight workers being filled rather than any per-parse
improvement. `dart`'s 4.94 ms maximum at 100 000 is a scheduler artifact of eight
workers on a fifteen-core host, not a parse: `dart`'s p99 at the same level is
2.0 us.
