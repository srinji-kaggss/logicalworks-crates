# The script language: one lexicon, one parser

Status: **Director direction of 2026-10-08; proposed, not implemented.**
Baseline: `a23ad5890`. Tracker: [#390](https://github.com/srinji-kaggss/logicalworks-crates/issues/390),
the ordered queue that names the issue making each rule true. Code-shaped
blocks are design examples, labelled `text`. **Open decision (the Director's):**
where the one parser lives — a default-off `script` feature of `lgwks_ast` (the
proposal below) or a sixth member the Director names; #383 waits on it.

## What the language is for

These crates are the public interface the estate writes most of its boilerplate
against. `script!` is the opinionated part of that interface: a small set of
loaded words, each of which compiles to code that already carries the estate's
rules. The model is WalkMe's internal vocabulary, where a few named building
blocks (`swt` for a function, `shoutout` for a modal) carry the product, taken
one level lower: each word here is an orchestration primitive.

The language serves two readers:

- **An AI author** writes it from the lexicon alone, next token by next token,
  without retrieval over the repository. The words have already made the design
  decisions, so writing the code is a matter of choosing which words to use.
- **A human reviewer** reads it line by line. Every line is one step a person
  can follow. When the author is wrong, either the compiler refuses the line and
  names the replacement, or the person reading the lines sees it. The code is
  the author's chain of thought, written so it can be checked.

Mistakes still happen. The language moves them to the two places where they are
cheapest to catch: the compiler, and a reader.

## The rules

One line each. The tracker names the issue that makes each one true.

| ID | Rule |
|---|---|
| SL-1 | **One lexicon.** Every word is one row of one table, holding six things: its spelling, its form, the `lgwks_bot::script` primitive it calls, what it refuses, the guarantees it carries and one example. The parser dispatches on that table. The crate docs, the README, `llms.txt` and the `ARCHITECTURE` map are generated from it or checked against it. |
| SL-2 | **One parser.** One function turns tokens into the script tree. The proc macro and every tool (gate, map, editor, an agent's dry compile) call that function. A second reader of the same language is a defect even when it agrees today. INV-BOT-21 already states this for inspection ("parses through `lgwks_ast` (never a second parser)"). |
| SL-3 | **One line, one call.** A line that is a word expands to exactly one call into `lgwks_bot::script`, under a structural step key that never contains a line number. This holds in `crates/lgwks-ast/src/script/parse/` (the labels) and `crates/lgwks-macros/src/emit.rs` (the calls). A line that is not a word is plain Rust passed through as written, and the map marks it as passthrough. |
| SL-4 | **Guarantees live in the words.** Bounded fan-out, a deadline, one idempotency key across retries, tenant scope and the absence of panics are properties of the word, so they hold wherever the word is used. The nine axes decide what a word must guarantee before it is admitted. They are a design requirement, not a checklist scored afterwards: once code is graded against an axis list, the list becomes the target and the code stops being the point. |
| SL-5 | **A refusal names its replacement.** The replacement is either a word or a typed failure. This holds in `crates/lgwks-ast/src/script/refuse.rs` and the parse refusals beside it. |
| SL-6 | **A run reads back as its script.** For each step, the trail records the word and the source text of its line, so a person can read an execution line by line against the script that produced it. |
| SL-7 | **Words are admitted like dependencies.** A new word needs six things: the recurring pattern it replaces, cited at three or more estate sites; its primitive; its refusals; its guarantees; a compile-fail test for each refusal; and a deterministic simulation of the primitive. Passthrough Rust that keeps recurring is the signal that a word is missing. |
| SL-8 | **The lexicon fits on one page.** An agent's entire context for writing a script is the generated lexicon, about 4k tokens. If it outgrows that, the language is sprawling and words should merge. |

## What exists at the baseline

`lgwks_macros` (`script!`) has thirteen block forms, listed by hand in
`crates/lgwks-macros/src/lib.rs`. Its parser is hand-rolled in three parts:

- `src/lines.rs` splits `proc_macro2` tokens into lines.
- The `src/emit/*.rs` files recognise words with string matches spread across
  them.
- `src/refuse.rs` holds the refusals as constants.

It meets SL-3 and SL-5. It misses SL-1, because the vocabulary lives in four
places. It misses SL-2, because a proc-macro crate can export only macros, so
no tool outside the macro can call its parser.

Other vocabularies that SL-1 must absorb or link to:

- Domain identifiers such as `"github::pr_status"`. They are declared with
  `domains!` (`crates/lgwks-bot/src/registry.rs`) and named by strings in
  `BotSpec` (`src/spec.rs`). Absorbed by #388: `observe <domain::id> of
  <target>` and `act <domain::id> on <target> with <value>` resolve the same
  strings through the host's registry, admitted at the flow's entry.
- Natural-language resolution of an utterance to a verdict (`src/language.rs`,
  `src/semantic.rs`, `src/session.rs`).
- The proposed task vocabulary of `task`, `all`, `map` and `watch` (DX-02 in
  `docs/declarative-orchestration.spec.md`).

Rust source is also read twice. `crates/lgwks-deps/src/scan.rs` reads it with
`syn`, and `lgwks_ast` reads it with tree-sitter (which
`crates/lgwks-bot/src/inspect.rs` uses). That is SL-2 one level down.

## Where the one parser lives

Three facts constrain the choice:

1. A proc-macro crate can export only macros.
2. The contract forbids a new crate unless the Director names one (INV-DEP-1
   fixes five members).
3. `proc_macro2` parses source text into the same token type a macro receives,
   and its `span-locations` feature reports line and column outside a macro.

Because of the third fact, one function from tokens to a tree can serve both
the macro and offline tools.

**Proposal.** Move the script parser and the lexicon into `lgwks_ast`, behind a
default-off `script` feature that pulls only `proc-macro2` and never
tree-sitter, so the macro's build cost does not grow. `lgwks_macros` becomes a
shim: it takes the macro input, calls `lgwks_ast::script::parse` and emits. The
`proc-macro2` edge is registered in `contract/APPROVED.toml` like any other
edge.

This supersedes "`lgwks_ast` is finished" in AGENTS.md and "never grow
`lgwks_ast`" in INV-DEP-1. The parser crate becomes the one parser. Both lines
change in the PR that moves the code, together with the register's
`frozen_surfaces` entry (INV-DEP-16).

Alternatives rejected:

- **Keep the parser in `lgwks_macros`.** No tool can call it, so every tool
  grows its own reader, which is the drift SL-2 forbids.
- **Put it in `lgwks_bot`.** The macro's build would pull in the async runtime.
- **Make a new `lgwks_script` crate.** It would be cleaner, but only the
  Director can name a sixth member. If the Director names it, the plan is the
  same with a different home.

The Rust-reader split (`syn` in scan, tree-sitter in `lgwks_ast`) is settled by
measurement, not preference: [ADR 0001](adr/0001-one-rust-reader.md) (#387)
measured fidelity, speed, peak RSS and hostile nesting on this repository's own
sources and chose `lgwks_ast`. Routing the scan, the losing reader, through it
is #408.

## How we know it works

Outcomes, not axis rows:

- **AI authoring.** `bench/ai-authoring` already runs fixed-model authors
  against the old `rt` surface and the `script` facade under one hidden oracle
  (INV-BOT-141..144). Add a cell whose only context is the generated lexicon.
  Report first-compile rate, compiler repairs, which refusals fired, and oracle
  verdicts against the existing cells.
- **Human review.** For the same trials, count the defects a reader found from
  the lines and the trail alone.
- **Size.** Lines per flow against the equivalent hand-written Rust.
- **Passthrough share.** The share of passthrough lines across estate scripts.
  It is a signal for SL-7 and never a gate, because a gated share would be
  gamed.

None of these has been measured for the lexicon-only cell yet.
