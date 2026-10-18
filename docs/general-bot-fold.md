# Folding the general recorder-and-replayer architecture into `lgwks_bot`

Pages of this guide: [scope and finding](general-bot-fold.md) · [what is new](general-bot-fold-new.md) · [recorder, settlement, language](general-bot-fold-language.md) · [semantic tier and mapping](general-bot-fold-semantic.md) · [reaching a page](general-bot-fold-reach.md) · [evasion and injection](general-bot-fold-evasion.md) · [sequence 1 to 5](general-bot-fold-sequence.md) · [sequence 6 to 14](general-bot-fold-frontier.md)

Status: **design record.** The first slice is implemented — `crates/lgwks-bot/src/interface.rs`
— and the rest is sequenced in §6. Nothing here changes the four verbs, the
authority model, or the ECS schedule.

## 1. What this document is for

A full browser-automation platform was described to this workspace: a
no-code recorder that turns arbitrary web interactions into a declarative,
versionable workflow graph, replayed by a headless browser, with extraction,
scheduling, run-diffing, and an optional LLM agent. It is a mature design and it
covers ground this workspace has not.

This document folds that design into `lgwks_bot`. "Fold" means: for each
subsystem, either name the artefact in this workspace that already *is* it, or
name the exact seam it becomes, or refuse it and say why. It is not a
comparison and not a plan to build a second platform. The failure mode it exists
to prevent is a parallel architecture — a second IR, a second interpreter, a
second authority model — arriving one subsystem at a time.

The fold is at the design level. No code is copied from the described platform
or from any of the projects whose published architecture it shares. If code is
ever taken, it goes through the rung-7 vendor path in `docs/dependency-doctrine.md`
and its licence is checked first.

The licence governing any given revision is the one in that crate's `Cargo.toml`
and its sibling `LICENSE` file: `Apache-2.0` for all four crates as of this
writing, and the root `README.md` indexes them. A move to a dual MPL-2.0 plus
commercial licence is **drafted and not landed** — there is no `LICENSING.md` in
this tree, and an earlier revision of this document cited one as though it
existed. Nothing here relicensed a published version, and a consumer should take
the manifest at their pinned revision as authoritative rather than this design
record.

## 2. The finding: the IR is already here

The described platform's intermediate representation is a `WhereWhatPair[]`:
a guard (`Where`: URL, cookies, selectors, with `$and` / `$or` / `$not`) paired
with an action pipeline (`What`: `goto`, `click`, `fill`, `scrape`, …).

That is this workspace's bot model, arrived at independently:

| Described platform | This workspace | Where |
|---|---|---|
| `WhereWhatPair` | a `ChainSpec`: a condition bound to an action | `crates/lgwks-bot/src/spec.rs` |
| `Where` boolean algebra | `Predicate::{And, Or, Not}` over a closed expression language | `crates/lgwks-bot/src/session/value.rs:287` |
| `Where` URL / cookie / selector guards | the conditions an `Observe` domain reports | `domain/*.rs` |
| `What[]` action pipeline | `Execute::execute_action`, one effect per call | `verb.rs` |
| `WorkflowInterpreter` state matcher | the ECS schedule: a condition is `Changed<T>` | `docs/bot-on-ecs.md` §1 |
| `WorkflowFile.meta` + graph | `FlowSpec` (vars, entry, nodes, edges, terminals) | `crates/lgwks-bot/src/session/graph.rs:385` |
| a step that dispatches to a subsystem | `NodeKind::Route { dispatch, fallback }` | `crates/lgwks-bot/src/session/validate.rs:184` |

So the first and largest correction to make is that **the fold does not need a
new IR**. A recorder's output is a `FlowSpec`; a scheduler's unit of work is a
`ChainSpec`; the interpreter is `Session` deciding the next node and the ECS
schedule executing it. Adding a second document format here would be the exact
duplication `AGENTS.md` calls a defect, and it would split the audit story: two
IRs means two answers to "what did this run actually do".
