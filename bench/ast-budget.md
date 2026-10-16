# The `lgwks_ast` parse-budget rig

Part of the [measurement rigs](README.md); the adversarial rows, markdown and the
fan-out tiers are in [ast-adversarial.md](ast-adversarial.md).

Per-grammar parse throughput, peak resident set size, and the share of a checked
parse spent in the validation walk, measured against the three grammars' inputs a
hostile file actually carries. Produced by
`crates/lgwks-ast/examples/parse_budget.rs`; the committed evidence is
[`ast-budget.tsv`](ast-budget.tsv), 224 rows.

```sh
cargo run --release -p lgwks_ast --features full --example parse_budget -- --help-ish
# one process per (grammar, shape) and per tier, because peak RSS is a process
# high-water mark and one process over everything could only report the widest:
AST_BUDGET_SHAPE_BYTES=262144 AST_BUDGET_TIMEOUT=120 \
  scripts/measure-ast-budget.sh /tmp/lgwks-ast-budget
```

**Machine and toolchain.** Apple M5 Pro, 15 cores, macOS 27.0, rustc 1.99.0,
release profile. One machine, one run: these are absolute numbers for this host,
not a cross-platform claim. The process floor — the same binary, all 28 grammar
tables linked in, parsing nothing — is **2.1 MiB**, and every "over floor" figure
below is that subtracted, because the binary carries every grammar whether or not
this run used it.

## Four shapes per grammar, at the crate's own byte ceiling

| shape | what it is | why it is a different failure |
|---|---|---|
| `representative` | the grammar's own valid source tiled to 2 MiB | the number a tool sees on real code |
| `nested` | a delimiter pair (or an indentation run) nested as deep as the budget allows | the input `MAX_AST_DEPTH` exists for |
| `longline` | the representative source with its newlines removed | one line of megabytes; the worst parser case in the set |
| `unbalanced` | the nested openers with no closers | forces recovery at the deepest level |

`nested` and `unbalanced` are measured at **256 KiB**, not the 2 MiB ceiling, and
that is a measurement finding rather than a convenience: at 256 KiB of nested
braces the **Dart** grammar takes **97.5 seconds** in the parser, and at 2 MiB it
did not finish in 120 s. The two ceilings are named separately in
`ast-budget.tsv` (`bytes` on every row) so a smaller figure is never read as the
ceiling's. Representative and `longline` are at the full 2 MiB.

## Throughput and peak RSS at 2 MiB, per grammar

`parse` is bare `tree_sitter::Parser::parse`; `checked` is the whole
`try_parse`; `walk share` is the validation walk's share of parse-plus-walk.

| grammar | parse MB/s p50 | p99 | checked MB/s p50 | walk share | peak RSS MiB | over floor |
|---|---:|---:|---:|---:|---:|---:|
| `solidity` | 17 | 17 | 14 | 25% | 100 | 98 |
| `yaml` | 16 | 16 | 15 | 8% | 137 | 136 |
| `rust` | 12 | 11 | 10 | 24% | 198 | 197 |
| `c` | 11 | 10 | 9 | 21% | 220 | 218 |
| `cpp` | 11 | 11 | 9 | 19% | 220 | 218 |
| `css` | 11 | 9 | 8 | 18% | 170 | 169 |
| `go` | 10 | 9 | 8 | 18% | 146 | 144 |
| `python` | 10 | 9 | 8 | 18% | 203 | 202 |
| `typescript` | 9 | 8 | 7 | 22% | 240 | 238 |
| `bash` | 8 | 8 | 7 | 15% | 218 | 216 |
| `csharp` | 8 | 7 | 7 | 18% | 330 | 328 |
| `haskell` | 8 | 7 | 6 | 17% | 301 | 299 |
| `dart` | 7 | 7 | 6 | 19% | 325 | 323 |
| `elixir` | 7 | 6 | 5 | 16% | 277 | 275 |
| `java` | 7 | 6 | 6 | 18% | 196 | 195 |
| `tsx` | 7 | 6 | 6 | 20% | 286 | 285 |
| `nix` | 6 | 5 | 5 | 12% | 114 | 112 |
| `scala` | 6 | 6 | 6 | 16% | 317 | 316 |
| `hcl` | 5 | 4 | 4 | 18% | 319 | 317 |
| `swift` | 5 | 4 | 5 | 20% | 361 | 359 |
| `html` | 4 | 4 | 3 | 10% | 153 | 152 |
| `json` | 4 | 3 | 3 | 37% | 294 | 292 |
| `kotlin` | 4 | 4 | 3 | 20% | 264 | 262 |
| `markdown` | 4 | 4 | 3 | 20% | 460 | 458 |
| `ruby` | 4 | 4 | 3 | 15% | 723 | 721 |
| `javascript` | 3 | 1 | 2 | 14% | 253 | 252 |
| `php` | 3 | 2 | 3 | 10% | 273 | 271 |
| `lua` | 2 | 2 | 2 | 19% | 310 | 309 |

**The documented ceiling: 723 MiB resident, for one parse of a 2 MiB file with the
Ruby grammar.** That is the worst grammar at the crate's own byte ceiling, and it
is what the crate's own ceiling costs in resident memory. `ruby` is 6.4x the
cheapest (`solidity`, 100 MiB) for the same 2 MiB, so a caller cannot size a
parser from the input alone — it has to name the grammar.

**The validation walk is 8%–37% of a checked parse on clean source**, and the
share is a *floor*, not a typical figure: it is measured on the walk the public
`inspect_ast` performs under a node cap, while the checked parse's own walk also
carries the depth cap, so it is cheaper. The walk is the larger share where the
tree is many nodes for few bytes — `json` at 37% is 1.5 M nodes in 2 MiB, while
`yaml` at 8% is a much smaller tree for the same bytes.
