# Machine-checked specification of the spec/condition layer

`bot-spec.sml` is a HOL4 script that states and proves seven theorems about the
loop-free boolean condition language of the `Evaluate` verb, its budgeted
evaluator, and the capability gate that turns conditions into fired entries and a
replayable record. `proof-run.log` is the unedited output of running it.

## What is proved

| Name | Statement | What it buys |
|---|---|---|
| `EVAL_N_AGREE` | `bdepth e ≤ n ⇒ eval_n e env n = SOME (eval e env)` | the budgeted evaluator is the unbudgeted one, once the budget covers the tree |
| `T1` | `∀e env. ∃n v. eval_n e env n = SOME v` | **totality**: every loop-free condition decides at some finite budget |
| `T2_MONO` | `eval_n e env n = SOME v ∧ n ≤ m ⇒ eval_n e env m = SOME v` | a verdict, once reached, is not lost by a larger budget |
| `T3` | `eval_n e env n = SOME v ⇒ (eval e env ⇔ v)` | a budgeted verdict is the true verdict, not a truncation artefact |
| `T4` | `MEM e (fired g env es) ⇒ g e.cap` | **gate soundness**: every entry that fires is one the grant permits |
| `T5` | `(∀c. g c ⇒ g' c) ∧ MEM e (fired g env es) ⇒ MEM e (fired g' env es)` | the gate is monotone in the grant — a wider grant fires a superset |
| `T6` | `replay (record g env es) = MAP (λe. e.action) (fired g env es)` | **record/replay agreement**: replaying a tick performs exactly what fired |
| `T7` | `∀env n. ∃e. ∀v. evalL_n e env n ≠ SOME v` | the loop constructor admits no value at any budget — see the scope note below |

`T4` and `T6` are the two that carry weight. `T4` is the gate's soundness: an
entry cannot fire without the capability it declares, for any grant, any
environment, any entry list. `T6` is what makes a recorded tick replaysafe — the
replay is not merely consistent with the tick, it is the fired list's actions in
order.

`T7` is not a result about the shipped language. It exists to delimit `T1`:
`LExpr` extends `BExpr` with `LLoop`, which evaluates its body and then loops
forever, and `T7` proves that no `LLoop`-rooted expression yields a value at any
finite budget. The loop-free totality in `T1` is therefore a property of the
*loop-free* language and does not survive the extension — which is the formal
reason the crate needs loop budgets rather than total functions.

## What is *not* proved — the refinement gap

**This is not a refinement proof, and it should not be read as one.** The
theorems are about `bot-spec.sml`'s own datatypes — `BExpr`, `Entry`, `LExpr`,
`Grant = string -> bool`, `Env = num -> num` — which are a hand-written abstract
model of the layer, not the Rust. Nothing here proves that
`crates/lgwks-bot/src/spec.rs` implements the model. There is no
`automata`-style or `cake`-style refinement relation, no extraction, no
translation validation, and no proof about any Rust program.

The honest form of the claim is therefore two-part, and only the first part is a
theorem:

1. **Proved:** the *design* has these properties. Given the model above, `T1`–`T7`
   hold by kernel-checked proof.
2. **Argued, not proved:** the *implementation* cannot express the states that
   would break them. `Cap` is a newtype over `Cow<'static, str>` rather than a
   `String` that could be empty or malformed; `Auth`'s constructor is
   crate-private, so a proof cannot be forged; `Grants` is written only in
   `assemble`, so the grant cannot widen or narrow under a running bot. Those are
   type-construction arguments a reader can check, and they are what connects the
   model to the code — not a proof.

A bug in the model is a bug in what is proved. The model is a claim about the
design, written by the same process that wrote the design, and the kernel
guarantees only that the claim follows from the model's own definitions.

Where the model deliberately abstracts:

- `Contains` over strings is represented as an opaque `BLit`. The model is about
  the shape of evaluation, not about string search, so the shipped `Contains`
  evaluator is not covered by anything here.
- `Env` is total (`num -> num`), so "a check reads an undefined slot" is not a
  state the model can be in. That is an assumption, and the Rust's equivalent
  freedom is not modelled.
- `action` is a `num`. Effects are not modelled at all, so nothing here says
  anything about what an action does once it fires.
- Capability *shortfall* (the `Deficit`/`Shortage`/`Demand` reporting, and the
  derived repair) is not modelled. `T4` is about the gate's soundness, not about
  how a denial is reported.
- The runtime, supervisor, scheduler and process layer are entirely absent. The
  `bench/` measurements in this repository are the evidence for those, and they
  are measurements, not proofs.

## Reproducing

The script opens no theory segment (`new_theory`) and exports nothing
(`export_theory`), so it evaluates entirely in memory and writes into no
signature store. It is run as a plain script:

```sh
/path/to/HOL/bin/hol < proofs/bot-spec.sml
```

`proof-run.log` was produced by HOL4 `Trindemossen 2`, built from source commit
`ab2229321a87364f9910f2fc5fcc9ad8217a2325`. The build is not vendored here;
`HOL4 source:` at the top of the script records the commit the log corresponds to.

**No theorem-fabricating primitive is used anywhere in the file.** There is no
`mk_thm`, no `new_axiom`, no `cheat`, and no `mk_oracle_thm`. Every theorem is
closed by genuine tactics and checked by the HOL4 kernel at run time — which is
why the log, not the source comment, is the evidence. That claim is greppable:

```sh
grep -nE 'mk_thm|new_axiom|cheat|mk_oracle_thm|OBJ|mk_thm' proofs/bot-spec.sml
```

returns nothing.

The log ends with a self-check: each theorem is re-checked against its statement
and printed as `[VERIFIED] Tn`, followed by `ALL_SEVEN_THEOREMS_PROVEN`.

### One warning in the log, stated rather than omitted

Line 88 of `proof-run.log` carries:

```
<<HOL warning: Context.snapshot: ambient context read while a running proof was running (in scratch)>>
```

This is HOL4's hygiene warning that a proof read ambient context (the scratch
simpset) rather than only its own. It affects proof *hygiene*, not soundness: the
kernel still checked every inference, and the warning appears between `T2_MONO`
and `T3`, both of which are `[VERIFIED]` in the self-check. It is recorded here
because a reader diffing the log against this table should not have to wonder
whether it was noticed.

## Relationship to the rest of the repository

This directory is evidence, not a build input. Nothing in the crate compiles it,
no gate reads it, and it is not on the road to a release — it is here so the
design claims in `docs/guides/lgwks-bot/` can be checked against something
stronger than prose where that is honestly possible, and so the places where it
is not possible are written down.
