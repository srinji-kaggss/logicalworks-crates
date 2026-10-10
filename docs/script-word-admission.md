# Admitting a word to the `script!` language

A word is admitted the way a dependency is
(`skills/lgwks-dependency-admission/SKILL.md`): from evidence, written down,
and checked by the build rather than by a reviewer's memory. This page is the
procedure SL-7 of `docs/script-language.spec.md` names. The language grows
when passthrough Rust keeps recurring, never because a word would be nice.

The one table is `LEXICON` in `crates/lgwks-ast/src/script/lexicon.rs`. The
parser dispatches on it, and the README table, the crate documentation table
and `docs/script-lexicon.md` are rendered from it and checked against it by
`lgwks_macros`'s `lexicon_docs` tests. A word that is not a row does not
exist, and a row is not admitted until every check below holds.

## 0. First, try not to add a word

A recurring passthrough line has three cheaper answers, in this order:

1. **A function in `lgwks_bot::script`**, called from a plain Rust line. A
   word earns its row only when it carries a guarantee a call cannot: a
   scope, a step key, a bound, an admission at the flow's entry.
2. **An existing word's form.** `each x in xs, at most (limit) at once:` is a
   second form of `each`, not a second word.
3. **A merge.** SL-8 caps the generated page at about 4,000 tokens
   (`PAGE_TOKENS` in `crates/lgwks-macros/src/lexicon_docs.rs`). If a new
   row would push it over, two words should become one.

## 1. Cite three estate sites

Run the census (section 7) and name three or more places the shape recurs as
passthrough, each as `path:line` in the PR body. Fewer than three is taste,
not evidence. A site in a test or a benchmark counts only if it is the shape
a user would write.

## 2. Write the row

Every field of `Word` is required, and each has a check:

| Field | What it holds | What checks it |
|---|---|---|
| `kind` | a new `Kind` variant and its `spelling()` | a word with a map entry needs the same variant in `lgwks_bot::script::StepKind`, because the macro emits `StepKind::<Kind's Debug>`; the build fails otherwise |
| `position` | `Flow`, `Block`, `Line` or `Inline` | the parser's dispatch |
| `primitive` | the `lgwks_bot::script` item the word calls, or `none: <Rust>` | review; SL-3 requires exactly one call per line |
| `forms` | each written form and what it means | rendered into all three tables |
| `refuses` | what the word itself refuses | rendered; each needs a case (step 3) |
| `guarantees` | each claim, its axis, and its `Evidence` | step 4 |
| `example` | one complete script using the word | expanded by the real macro in `lgwks_macros`'s `lexicon_docs` tests |

Then render the tables: update the README and `lib.rs` tables to the text
the failing `lexicon_docs` test prints, and regenerate the page with
`LGWKS_WRITE_LEXICON_PAGE=1`.

## 3. A refusal case for each refusal

Each entry in `refuses` gets a row in `REFUSALS`
(`crates/lgwks-macros/src/tests.rs`): a case name, a script that must not
expand, and the text the refusal must contain. A refusal names its
replacement (SL-5): the form that would have been accepted, or the typed
failure to use instead. A guarantee that rests on a refusal cites the case
by name with `Evidence::Refusal`.

## 4. A test for each guarantee

A guarantee is a design requirement stated before the code (SL-4), and the
axis it names is the reason it is there, not a box to fill. Each one cites
the test that would fail without it:

- `Evidence::Refusal(case)` for a promise the parser keeps by refusing.
- `Evidence::Sim { file, test }` for a promise the runtime keeps, in a
  deterministic simulation (step 5).

`lgwks_macros` refuses a guarantee whose cited test does not exist in its
file, so a row cannot cite evidence that was never written.

## 5. A deterministic simulation of the primitive

The word's runtime gets a `sim_*` family under `crates/lgwks-bot/tests/it/`:
flows the real macro expanded, on a real `Host`, with every input drawn from
a seed, an oracle computed independently of the code under test, and a
sweep through `sim::assert_replays` so the same seed gives the same trace
hash. `sim_script_domains.rs` (#388) is the pattern: four families of 1,024
seeds, one of them comparing the script's trace against the `BotSpec` path
over the same registry.

The parser side gets the same treatment in
`crates/lgwks-ast/src/script/sim_script.rs`: the drawn scripts must say the
new word (`say(Kind::..)`), so the "every word is said" family fails until
the generator writes it.

## 6. Wire it and land it

- Emit the call in `crates/lgwks-macros/src/emit.rs`, and the map entry so
  the architecture map and the run's trail name the step (SL-6).
- Add the rule to `INVARIANTS.md` in one line, or extend the line it
  belongs to; the file stays at 150 lines or fewer.
- One PR per word, closing its issue, with the sites, the refusal cases, the
  guarantees and their tests listed in the body, and the gate green.

## 7. The passthrough census

`lgwks-ast script map --json [PATH...]` reads every `script!` under `PATH`
through the one parser and prints its map. Each flow carries a `passthrough`
list of `{"line", "text"}`: every line no word reads (a plain Rust statement,
a `let` that opens no block, or a `together:` branch's value) with no `run`,
`observe` or `act` anywhere inside it. The compiled `ARCHITECTURE` carries
the same list (`FlowShape::passthrough`), and
`crates/lgwks-bot/tests/it/script_map_parity.rs` holds the two equal.

That list is what step 1 counts. A shape is the line with its names and
literals blanked, so `let a = site.get(id)` and `let b = page.get(n)` are one
shape. `scripts/passthrough-census.py` prints the table:

```sh
cargo run --locked --release -p lgwks_ast --features tool --bin lgwks-ast -- \
    script map --json . | python3 scripts/passthrough-census.py
```

The census measures and never gates: a gated passthrough share would be
gamed by moving code out of scripts, which is the opposite of the goal. The
spec lists it among the outcome measures for that reason.
