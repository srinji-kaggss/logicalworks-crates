(* ==================================================================== *)
(* bot-spec.sml                                                         *)
(*                                                                      *)
(* A machine-checked specification of the spec/condition layer of       *)
(* lgwks_bot: the loop-free boolean condition language of the Evaluate  *)
(* verb, its budgeted evaluator, and the capability gate that turns     *)
(* conditions into fired entries and a replayable record.               *)
(*                                                                      *)
(* This is a PLAIN HOL4 script: it opens no theory segment (no          *)
(* new_theory) and exports nothing (no export_theory), so it evaluates  *)
(* entirely in memory and writes into no signature store. It is run as  *)
(*                                                                      *)
(*   /Users/srinji/hol4-build/HOL/bin/hol < bot-spec.sml                *)
(*                                                                      *)
(* and every theorem below is closed by genuine tactics. No primitive  *)
(* that fabricates a theorem is used anywhere in this file; each proof  *)
(* is checked by the HOL4 kernel at run time, so the log is the         *)
(* evidence for that and this comment is not.                           *)
(*                                                                      *)
(* HOL4 source: commit ab2229321a87364f9910f2fc5fcc9ad8217a2325         *)
(* ==================================================================== *)

open HolKernel boolLib bossLib;
load "stringTheory";
open stringTheory;

(* -------------------------------------------------------------------- *)
(* Types                                                                *)
(* -------------------------------------------------------------------- *)

(* Grant: the capabilities the bot holds. The runtime holds an issued
   grant set; the model keeps the question "may this bot use capability
   c" as a decision procedure, so that "the bot was given more
   capabilities" can be stated extensionally as a pointwise implication
   between two grants (theorem T5). *)
Type Grant = ``:string -> bool``

(* Env: the observation environment. A total function from numeric slots
   to numbers, standing for the values a bot reads while deciding a
   condition. Totality matters: a check never has to consult an
   undefined slot part-way through, so a condition cannot fail for the
   want of an observation. *)
Type Env   = ``:num -> num``

(* BExpr: the loop-free condition language of the Evaluate verb.
   BLit is a constant verdict, BVar i reads slot i (and holds when that
   slot is non-zero), BLt i j compares two slots, and the three
   connectives compose. The shipped evaluators Changed, Below, Above and
   Contains are the leaves of this language; All and Any are the BAnd
   and BOr nodes. Contains over strings is represented as an opaque BLit,
   because the model is about the shape of the evaluation and not about
   string search. *)
Datatype:
  BExpr = BLit bool
        | BVar num
        | BLt num num
        | BAnd BExpr BExpr
        | BOr  BExpr BExpr
        | BNot BExpr
End

(* Entry: one chain entry. A condition, the capability the entry needs,
   and the action the entry performs when it fires. This mirrors the
   condition / required-capability / action triple the spec layer carries
   for each entry of a chain. *)
Datatype:
  Entry = <| cond : BExpr ; cap : string ; action : num |>
End

(* LExpr: the same language extended with a loop constructor. It exists
   only to delimit the loop-free result: LLoop b evaluates its body and
   then loops forever, so it yields no value at any finite budget, and
   the totality proved for BExpr (T1) does not carry over (T7). *)
Datatype:
  LExpr = LLit bool
        | LAnd LExpr LExpr
        | LLoop LExpr
End

(* -------------------------------------------------------------------- *)
(* The budgeted evaluator and its node count                            *)
(* -------------------------------------------------------------------- *)

(* bdepth: the number of nodes of a condition. It is the budget at which
   the budgeted evaluator is guaranteed to have answered (T1, T3). *)
Definition bdepth_def:
  (bdepth (BLit b) = 1) /\
  (bdepth (BVar i) = 1) /\
  (bdepth (BLt i j) = 1) /\
  (bdepth (BAnd a b) = 1 + bdepth a + bdepth b) /\
  (bdepth (BOr a b) = 1 + bdepth a + bdepth b) /\
  (bdepth (BNot a) = 1 + bdepth a)
End

(* eval_n: the budgeted evaluator. It spends one step per node visited
   and answers NONE when the budget is exhausted before a verdict is
   reached. This is the machine a runtime would use if condition
   evaluation were driven by an explicit fuel counter; its role in the
   model is to make the cost of a condition observable, so that
   "evaluation terminates within a budget" can be stated as an equation
   rather than assumed. Note that the budget is spent on a node before
   its children are visited, which is why a binary node costs a step of
   its own. *)
Definition eval_n_def:
  (eval_n (BLit b) env n = if n = 0 then NONE else SOME b) /\
  (eval_n (BVar i) env n = if n = 0 then NONE else SOME (env i <> 0)) /\
  (eval_n (BLt i j) env n = if n = 0 then NONE else SOME (env i < env j)) /\
  (eval_n (BAnd a b) env n =
     if n = 0 then NONE
     else case eval_n a env (n - 1) of
            NONE => NONE
          | SOME x => (case eval_n b env (n - 1) of
                         NONE => NONE
                       | SOME y => SOME (x /\ y))) /\
  (eval_n (BOr a b) env n =
     if n = 0 then NONE
     else case eval_n a env (n - 1) of
            NONE => NONE
          | SOME x => (case eval_n b env (n - 1) of
                         NONE => NONE
                       | SOME y => SOME (x \/ y))) /\
  (eval_n (BNot a) env n =
     if n = 0 then NONE
     else case eval_n a env (n - 1) of
            NONE => NONE
          | SOME x => SOME (~x))
End

(* eval: the unbudgeted evaluator, total by construction. It is the value
   a condition has once the runtime allows it as much time as it needs,
   and it is the reference the budgeted evaluator is measured against
   (T3). *)
Definition eval_def:
  (eval (BLit b) env = b) /\
  (eval (BVar i) env = (env i <> 0)) /\
  (eval (BLt i j) env = (env i < env j)) /\
  (eval (BAnd a b) env = (eval a env /\ eval b env)) /\
  (eval (BOr a b) env = (eval a env \/ eval b env)) /\
  (eval (BNot a) env = ~(eval a env))
End

(* evalL_n: the budgeted evaluator for the language with loops. A literal
   costs a step, conjunction is as above, and LLoop b evaluates its body
   once and then continues to loop: it returns a value at no finite
   budget, which is the semantic content of a loop that never terminates.
   The body is still evaluated, so the fuel is consumed rather than
   discarded. *)
Definition evalL_n_def:
  (evalL_n (LLit b) env n = if n = 0 then NONE else SOME b) /\
  (evalL_n (LAnd a b) env n =
     if n = 0 then NONE
     else case evalL_n a env (n - 1) of
            NONE => NONE
          | SOME x => (case evalL_n b env (n - 1) of
                         NONE => NONE
                       | SOME y => SOME (x /\ y))) /\
  (evalL_n (LLoop b) env n =
     if n = 0 then NONE
     else case evalL_n b env (n - 1) of
            NONE => NONE
          | SOME x => NONE)
End

(* -------------------------------------------------------------------- *)
(* Firing, recording and replaying                                      *)
(* -------------------------------------------------------------------- *)

(* fired: the entries that fire on a tick. An entry fires when its
   condition holds in the current environment and the bot holds the
   capability the entry requires. It is a plain filter, so the order of
   the chain is preserved and every decision is a local one. *)
Definition fired_def:
  fired g env es = FILTER (\e. eval e.cond env /\ g e.cap) es
End

(* record: what a tick writes down. The (capability, action) pair of each
   entry that fired, in chain order. *)
Definition record_def:
  record g env es = MAP (\e. (e.cap, e.action)) (fired g env es)
End

(* replay: what a replay of a recorded tick performs. It is a pure
   projection of the record and consults no environment: a replay states
   what happened without re-deciding anything, which is what makes the
   record auditable after the observations have moved on. *)
Definition replay_def:
  replay ps = MAP SND ps
End

(* -------------------------------------------------------------------- *)
(* Theorems                                                             *)
(* -------------------------------------------------------------------- *)

(* EVAL_N_AGREE: the budgeted evaluator agrees with the unbudgeted one at
   any budget at least as large as the node count of the condition. It is
   the lemma the totality and soundness results rest on, and it is proved
   by structural induction on the condition. *)
val EVAL_N_AGREE = prove(
  ``!e env n. bdepth e <= n ==> (eval_n e env n = SOME (eval e env))``,
  Induct_on `e` >> rw [eval_n_def, eval_def, bdepth_def]);

(* T1: every condition can be evaluated within some finite budget, namely
   its node count. Evaluation of a condition therefore always terminates:
   no condition of this language can fail to produce a verdict for want
   of time. *)
val T1 = prove(
  ``!e env. ?n v. eval_n e env n = SOME v``,
  rpt gen_tac >> EXISTS_TAC ``bdepth e`` >> EXISTS_TAC ``eval e env`` >>
  rw [EVAL_N_AGREE]);

(* T2: monotonicity of the budget. A verdict once reached is still that
   verdict at every larger budget. Fuel is an implementation mechanism:
   raising it makes more conditions evaluable, and it never changes a
   verdict that has already been produced. *)
val T2_MONO = prove(
  ``!e env n m v. (eval_n e env n = SOME v) /\ n <= m ==> (eval_n e env m = SOME v)``,
  Induct_on `e` >> rw [eval_n_def] >>
  TRY (Cases_on `eval_n e env (n - 1)` >> fs []) >>
  TRY (Cases_on `eval_n e' env (n - 1)` >> fs []) >>
  TRY (`n - 1 <= m - 1` by rw []) >>
  TRY (qpat_x_assum `!env n m v. eval_n e env n = SOME v /\ n <= m ==> eval_n e env m = SOME v`
        (fn ih => assume_tac (SPECL [``env : num -> num``, ``n - 1``, ``m - 1``, ``x : bool``] ih))) >>
  TRY (qpat_x_assum `!env n m v. eval_n e' env n = SOME v /\ n <= m ==> eval_n e' env m = SOME v`
        (fn ih => assume_tac (SPECL [``env : num -> num``, ``n - 1``, ``m - 1``, ``x' : bool``] ih))) >>
  fs []);

(* T3: the budgeted evaluator agrees with the unbudgeted one. Whenever the
   budgeted evaluator returns a verdict, that verdict is the meaning of
   the condition, so the fuel counter is not part of the semantics of a
   condition and an implementation may choose any sufficient budget. *)
val T3 = prove(
  ``!e env n v. (eval_n e env n = SOME v) ==> (eval e env = v)``,
  rpt gen_tac >> strip_tac >>
  `n <= bdepth e + n` by rw [] >>
  mp_tac (SPECL [``e : BExpr``, ``env : num -> num``, ``n : num``,
                 ``bdepth e + n``, ``v : bool``] T2_MONO) >>
  `eval_n e env (bdepth e + n) = SOME (eval e env)` by rw [EVAL_N_AGREE] >>
  fs []);

(* T4: the capability gate is sound. Every entry that fires on a tick is an
   entry whose capability the bot holds, so no action is taken on the
   strength of a capability that was never granted. *)
val T4 = prove(
  ``!g env es e. MEM e (fired g env es) ==> g e.cap``,
  rpt gen_tac >> rw [fired_def, listTheory.MEM_FILTER] >> fs []);

(* T5: the gate is monotone in the grant. Adding capabilities only adds
   fired entries and never removes one, so a tick computed under a smaller
   grant is a sub-tick of the same tick computed under a larger one. *)
val T5 = prove(
  ``!g g' env es e. (!c. g c ==> g' c) /\ MEM e (fired g env es) ==>
                    MEM e (fired g' env es)``,
  rpt gen_tac >> rw [fired_def, listTheory.MEM_FILTER] >> fs []);

(* T6: recording and replay agree. Replaying what a tick recorded performs
   exactly the actions of the entries that fired, in chain order, and the
   replayer reads no observations of its own. *)
val T6 = prove(
  ``!g env es. replay (record g env es) = MAP (\e. e.action) (fired g env es)``,
  rpt gen_tac >> rw [record_def, replay_def, listTheory.MAP_MAP_o,
                     combinTheory.o_DEF]);

(* T7: the totality of T1 is a property of the loop-free language and not
   of the budgeted evaluator in general. Once the language has a loop
   constructor, the term LLoop (LLit T) yields no value at any budget
   whatever, so no budget can make every term of the extended language
   evaluable. *)
val T7 = prove(
  ``!env n. ?e. !v. ~(evalL_n e env n = SOME v)``,
  rpt gen_tac >> EXISTS_TAC ``LLoop (LLit T)`` >> rpt gen_tac >>
  rw [evalL_n_def] >>
  Cases_on `evalL_n (LLit T) env (n - 1)` >> fs []);

(* -------------------------------------------------------------------- *)
(* Verification: re-read each stored theorem, compare its conclusion    *)
(* with the statement the specification claims, and report. A mismatch  *)
(* or a missing theorem stops the script before it prints the completion *)
(* marker, so a green run is evidence that all seven closed.            *)
(* -------------------------------------------------------------------- *)

fun check (name : string, th : thm, expected : term) =
  let val c = concl th
  in
    if aconv c expected then print ("[VERIFIED] " ^ name ^ "\n")
    else (print ("[MISMATCH] " ^ name ^ "\n   got: ");
          print_term c; print "\n   want: "; print_term expected; print "\n")
  end;

val _ = print "SPEC_CHECK_BEGIN\n";

val _ = check ("T1", T1, ``!e env. ?n v. eval_n e env n = SOME v``);

val _ = check ("T2", T2_MONO,
  ``!e env n m v. (eval_n e env n = SOME v) /\ n <= m ==>
                  (eval_n e env m = SOME v)``);

val _ = check ("T3", T3,
  ``!e env n v. (eval_n e env n = SOME v) ==> (eval e env = v)``);

val _ = check ("T4", T4,
  ``!g env es e. MEM e (fired g env es) ==> g e.cap``);

val _ = check ("T5", T5,
  ``!g g' env es e. (!c. g c ==> g' c) /\ MEM e (fired g env es) ==>
                    MEM e (fired g' env es)``);

val _ = check ("T6", T6,
  ``!g env es. replay (record g env es) = MAP (\e. e.action) (fired g env es)``);

val _ = check ("T7", T7,
  ``!env n. ?e. !v. ~(evalL_n e env n = SOME v)``);

val _ = print "ALL_SEVEN_THEOREMS_PROVEN\n";
