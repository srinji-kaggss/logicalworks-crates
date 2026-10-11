# Admitting a word to `script!`

Status: the procedure for SL-7 of [the script language spec](script-language.spec.md)
(#389). It mirrors dependency admission
([`skills/lgwks-dependency-admission/SKILL.md`](../skills/lgwks-dependency-admission/SKILL.md)):
the language grows from evidence, not taste, and the default answer to "can I
add this word?" is no. Whether this page also becomes a skill is the Director's
decision.

## 0. First, try not to add anything

A word earns its place by replacing Rust that keeps being written. Before
proposing one:

- Read [the lexicon](script-lexicon.md). If an existing word, or a new form of
  one (a second written form under the same row, as `each` has), says it, use that.
- If the pattern is a library call with no orchestration in it (no fan-out,
  deadline, retry, key, tenant or effect), it stays passthrough Rust. Words are
  orchestration primitives, not shorthand.
- SL-8 caps the lexicon at one page, about 4k tokens. A word that does not fit
  must replace or merge another.

## 1. Evidence: three or more estate sites

Run the census (step 6) and cite **three or more** passthrough sites, by
`repo:path:line`, whose shape the word would replace. Sites in one flow, or
copies of one example, count once. The census table and its repository list go
in the PR.

## 2. The six things SL-7 requires

Each is a field of the word's row in `LEXICON`
(`crates/lgwks-ast/src/script/lexicon.rs`), or a test that row names:

| # | Requirement | Where it lives |
|---|---|---|
| 1 | The recurring pattern, cited at three or more sites | The PR body, from the census |
| 2 | Its primitive: the one `lgwks_bot::script` function a line of the word expands to (SL-3) | `Word::primitive`; the call is emitted by `crates/lgwks-macros/src/emit.rs` |
| 3 | Its refusals, each naming its replacement (SL-5) | `Word::refuses`; the messages in `crates/lgwks-ast/src/script/refuse.rs` |
| 4 | Its guarantees, each with an axis and the test that fails if it stops holding (SL-4) | `Word::guarantees`, each a `Guarantee { axis, claim, evidence }` |
| 5 | A compile-fail case per refusal | A case of `REFUSALS` in `crates/lgwks-macros/src/tests.rs`, cited as `Evidence::Refusal` |
| 6 | A deterministic simulation of the primitive | A seeded test or `band_family!` in a `sim_` file under `crates/lgwks-bot/tests/it/`, cited as `Evidence::Sim` |

The nine axes decide what a word must guarantee before it is admitted. They are
a design requirement, not a score (SL-4). An `Axis` variant no row names is dead
code that `-D warnings` refuses, so a word that first serves an axis adds that variant.

## 3. Write the row, then the reader

1. Add the `Kind` variant and its spelling. `Kind` is constructed only in
   `LEXICON`, so a variant with no row fails the build.
2. Add the `Word` row: kind, position (`Flow`, `Block`, `Line` or `Inline`),
   primitive, forms, refusals, guarantees and one whole example script.
3. Add the word's reader beside the others in
   `crates/lgwks-ast/src/script/parse/`. The parser dispatches on the row, so
   there is no second list to update (SL-2).
4. Emit the one call in `lgwks_macros`, under a structural step key that never
   contains a line number (SL-3), and give the map a `StepKind` for it in
   `crates/lgwks-bot/src/script/map.rs`, so `script map` and `ARCHITECTURE` show it.
5. Implement the primitive in `lgwks_bot::script`, reusing `std`, then
   `lgwks_std`, then `lgwks_bot`, before writing anything new.

## 4. Regenerate what the row feeds

The lexicon page, the README word table and the crate docs' block table are
generated from `LEXICON` or checked against it. Run the `lgwks_macros` tests and
commit what they render (`the_lexicon_page_is_rendered_from_the_lexicon`). A
word described in one place and not the other fails there.

## 5. Prove it

```sh
cargo nextest run --locked -p lgwks_macros           # rendering + every REFUSALS case
cargo nextest run --locked -p lgwks_bot --features full -E 'test(/^sim_script/)'
cargo run --locked -p lgwks_ast --no-default-features --features tool --bin lgwks-ast -- script check
```

The PR's End-to-end section quotes the compile-fail output of each refusal, the
simulation's same-seed trace hash, and the census before and after: the shape
the word replaces should drop.

## 6. The census

Passthrough that recurs across scripts is the signal that a word is missing.
`scripts/script-census.py` counts it by shape, from the one parser's map:

```sh
cargo build --locked --release -p lgwks_ast --no-default-features --features tool --bin lgwks-ast
(cd <repo> && <this repo>/target/release/lgwks-ast script map --json .) > <scratch>/<repo>.json
python3 scripts/script-census.py <repo>=<scratch>/<repo>.json [...] --top 25
```

A line is passthrough when it is inside a `script!`, is neither blank nor a
comment, and is not a line the map names (a flow header or any step header,
nested ones included). A shape keeps keywords, punctuation, method and macro
names and collapses other names, literals and argument lists, so
`let n = page.len()` and `let size = body.len()` count together. The share is a
signal, never a gate: a gated share would be gamed.

## Refusals of a proposal

| Refusal | Repair |
|---|---|
| Fewer than three independent sites | Leave it as passthrough, and cite the census when it recurs. |
| No orchestration in the pattern | It is a library call: keep it Rust, or propose an `lgwks_std` function. |
| A refusal with no compile-fail case | Add the `REFUSALS` case, and cite it as the guarantee's evidence. |
| A guarantee with no failing test | Write the simulation, or drop the claim. |
| The lexicon outgrows one page | Merge the word into an existing one as a form, or remove one. |
