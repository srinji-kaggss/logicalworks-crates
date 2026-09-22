# Frontier definition

Status: reference. What the state of the art actually is, and what it decides for
`lgwks_bot`. Companion to [`guidance-runner-spec.md`](guidance-runner-spec.md) and
[`estate-asset-inventory.md`](estate-asset-inventory.md).

## Why this exists

The bot's target is not "better than WalkMe". It is the best bot that can be
built. That is an unfalsifiable slogan on its own, so this document replaces it
with a definition: nine areas where the frontier is locatable, what is settled in
each, what is genuinely open, and — the only part that matters — the decision each
one forces on the design.

The method: nine parallel literature sweeps over a live search index, the OpenAlex
citation graph, and paper text, anchored on measured results rather than claims.
Every number below is from a fetched leaderboard, paper, or patent record. Where a
source was unreachable, the limit is stated rather than the gap papered over.

Two conventions. **Solved** means no longer a research problem — build against it
and do not spend budget re-deriving it. **Open** means the field does not know, so
a decision here is a bet and should be recorded as one.

---

## 1. Element grounding — the sketch is half right, and the half that is wrong is load-bearing

**Frontier claim.** Hand-weighted multi-vector scoring was never replaced on the
DOM; it is still the recall engine, with a learned model used only as a top-N
reranker. On pixels-only grounding, learned models won decisively. And no
confidence threshold anywhere can separate "the element moved" from "the element
is gone" — the frontier's answer is a typed outcome, not a score.

**Anchors.**

- Similo (ACM TOSEM 2022) — weighted multi-attribute similarity over 40 real
  popular sites, 598 cases: 72 failures against the XPath baseline's 146, i.e.
  **88.0% vs 75.6%**. Hand-weighted scoring on real drifted pages is measured.
- Similo LLM (arXiv 2310.02046) — 804 element pairs, 48 real applications:
  failures 70 → 39. The LLM is a **reranker over the weighted scorer's top-N**,
  not a replacement.
- ScreenSpot-Pro leaderboard (2026) — pixels-only went **18.9% → 82.7%** in about
  eighteen months. On pixels, learned won outright.
- Set-of-Mark (MSR 2023) — the settled interface: the model emits a *mark ID*,
  the DOM resolves it to an exact node. Selection is learned; identity is
  structural. No coordinate is ever emitted.
- False-heal study (Aug 2026, 133 scenarios, 4 multi-provider runs) — on
  **removed** elements the heuristic baseline false-heals 40.5–57.1% of the time.
  Deleted-element decoys score **[0.665, 0.955]**; true compound drift scores
  **[0.749, 0.874]**. The ranges overlap, so no threshold, margin, or filter
  separates them. LLM consensus was right 52/52 on surviving elements and
  **wrong 34/34 on deleted ones**. Declining happened once in 78.
- Patents: WalkMe **US10713068** and **US11720379** (priority 2019-09-19, expiry
  2039-09-19) and UiPath **US11200073B1** (expiry 2040-11-20, with EP/CN/JP/KR
  family members). Multi-vector element recognition is fenced by exactly two
  assignees.

**Solved.** Re-resolving a *moved* element given DOM or accessibility-tree access.
Multi-attribute weighted similarity plus a top-N semantic reranker reaches ~95% on
published real-page element-pair benchmarks. Pixel grounding of a *visible* element
is effectively solved. Set-of-Mark is the settled learned/structural interface.

**Open.** *Moved* versus *gone* is **provably unsolved by structural evidence** —
the distributions overlap, so absence detection is mathematically bounded rather
than merely untuned. There is no calibrated confidence that means anything.

**What it changes.** Three decisions, and the first is the most important thing in
this document.

1. **The resolver returns a typed three-way, and `ElementRef` is minted only from
   the resolved case.** `Resolved(ElementRef)` / `Ambiguous(Vec<Candidate>)` /
   `Absent`. Because the moved/gone distinction cannot be a scalar threshold, a
   healed or guessed element must have **no type-level path into `Execute`**. This
   is the proof-carrying pattern the crate already uses for authority, applied to
   observation — and it is the single most defensible idea available to this
   project.
2. **Recognition is a cascade, not a flat sum.** The hand-weighted multi-vector
   scorer is the deterministic default path — millisecond-scale, ~88% recall, zero
   tokens — and a learned reranker is consulted only over the top-N when the score
   margin is low. Never a model on the default path.
3. **If a model enters the loop, it returns a mark ID the resolver minted, never a
   coordinate.**

And one commercial flag: **obtain claim charts for the WalkMe 2019 family and
UiPath US11200073 before shipping a multi-vector scorer.** The architecture above
is the mitigation — the patent exposure is in the weighted-score internals, so
Set-of-Mark binding is the independently-derived path.

---

## 2. Benchmarks — the boards do not measure the thing the bot is for

**Frontier claim.** Short-horizon web benchmarks are saturated or statistically
unordered; long-horizon is genuinely low; and **no benchmark in the field isolates
UI drift on a stable task**.

**Anchors.**

- WebVoyager: 59.1% → 98.5%, scored by GPT-4V judgment on live sites, so it no
  longer orders the top tier. GAIA: 92.36% for a six-model ensemble, i.e. an
  orchestration artifact, not a model result.
- SWE-bench Verified convergence audit (ADMA 2026, 254 submissions) — the top ten
  share 285 successes, scaffold choice swings a model by up to **29.8 pp** against
  an 8.8-pp spread across the top thirty, and McNemar separates **none** of the 29
  adjacent top-thirty pairs at α=0.05.
- OSWorld 2.0 (2026) — 108 tasks, ~318 tool calls each. Best binary completion
  **20.6%** against 54.8% partial. The gap between partial and binary *is* the
  long-horizon failure rate. Agent S: **+9.37% absolute** over the OSWorld
  baseline from dual-input observation.
- HORIZON (2026, 3,100+ trajectories, human–judge κ=0.84) — long-horizon failure
  is a **compositional shift**, not a rate drop: planning and memory failures
  displace execution failures as horizon grows.
- Online-Mind2Web — most commercial agents *underperform* the academic SeeAct
  baseline from early 2024 on live sites.
- DynamicGUIBench (ACM MM 2026) — reframes the problem as a POMDP where the
  critical state occurs *between* observations and is unrecoverable from any
  single screenshot.

**Solved.** Nothing that matters here. The boards are instruments for the wrong
quantity.

**Open.** Robustness under UI drift has **no credible dedicated benchmark**. The
closest instruments inject *observable* anomalies, or measure hidden interstitial
dynamics. Nobody measures "the page changed underneath the automation, did the
agent notice and recover," with a success-rate delta attributable to the mutation.

**What it changes.**

- **Build the drift harness as the primary regression gate**, not as a nice-to-have.
  Mutation injection — element moved, renamed, restructured, removed — under a
  fixed task, reporting a success-rate delta attributable to each mutation class.
  This is a gap the whole field has, so owning it is leverage.
- **Report paired per-task outcomes and comparison-set resolution, never an
  aggregate alone**, per the SWE-bench audit protocol.
- **`Observe` binds to a temporal window with a monotonic sequence number, not a
  screenshot.** A single post-action frame is a POMDP observation, and the
  dominant long-horizon failures are downstream of that.

---

## 3. Agent architecture — the loop converged; the differentiation moved out of it

**Frontier claim.** The frontier converged on one loop shape — plain iterative
action-observation — with plan-execute surviving as a *mode grafted onto* it and
reflection demoted to a stop condition. The remaining differentiation is entirely
in the surrounding harness: context lifecycle, verification, permissioning,
handoff.

**Anchors.**

- Harness Engineering (Sept 2026) — source audit of 11 production harnesses,
  ~4M LOC. All eleven implement action-observation variants; nine are plain
  iterative; coordinator-worker is only ever an overlay. **No runtime imports
  LangChain, LangGraph or AutoGen, and none retrieves code with embeddings.**
- Sample More, Reflect Less (Aug 2026) — 36 paired token-matched comparisons with
  Holm correction: **no method reliably beats repeated sampling at equal token
  cost**, and all 18 self-inspection comparisons were negative.
- Beyond Compaction (2026) — typed, dependency-linked episode graph with a
  deterministic, LLM-free eviction policy; one session over 89 sequential tasks
  and 80M tokens with no measurable degradation.
- HAS-Bench (2026) — humans and agents as first-class graph participants with
  roles, permissions and **action authority**.

**Solved.** Loop topology. Observation space (structured interface for identity,
screenshot for change, element-index actions). Framework-versus-model — settled by
absence at scale. Self-reflection as a general accuracy lever — settled negative
when self-administered at equal tokens.

**Open.** Long-horizon hidden state (losing constraints across ~318-step horizons)
is the headline failure, not GUI control. **Escalation calibration** — "guess
rather than ask the user" is named by benchmark authors as a top failure mode, and
no production harness ships a policy for *when* to escalate. Interruption and
resumption have no cross-system semantics, and no harness treats **authority** as
the thing being suspended.

**What it changes.**

- **Do not add a `Plan` verb.** Plan-execute is a mode over an action-observation
  base. A plan belongs in `Query` as a *candidate record* under the existing
  `(Auth, input)` seam, with `GrantSet::issue` remaining the only mint.
- **Do not build self-critique into `Evaluate`.** The measured verdict on
  self-inspection at equal tokens is negative-to-neutral. Critique belongs in an
  outer verify-on-stop guard whose verifier is mechanical or tool-grounded —
  which is exactly `forge-md-runtime`'s advisory-versus-promoted split. That makes
  the reflection question an *evidence* question rather than a taste question.
- **Context is a dependency-linked typed episode projection, not a transcript**,
  with deterministic eviction rather than summarization-compaction.
- **The frontier's top open problem is the one `GrantSet` already models.**
  "Guess rather than ask" is an authority-escalation question. A `Query` that
  *requests a grant* makes asking a first-class machine-checkable outcome, and
  human takeover becomes a grant lifecycle event rather than the dialogue state
  machine this architecture deliberately does not have.

The ECS change-detecting schedule is a **stricter factoring** than any of the
eleven audited production harnesses: it separates observation from evaluation
deterministically rather than by prompt.

---

## 4. Task mining and record-and-replay — and the finding that decides the evidence model

**Frontier claim.** The frontier abandoned "infer the one true workflow from one
trace". The defensible position is: induce a program from a **corpus**, admit each
ordering edge only with an auditable evidence tuple, validate on held-out traces,
and **refuse to emit when intent is under-determined**. And invariants that were
never in the trace are recovered not by mining but by declaring typed
preconditions and postconditions against the system of record.

**Anchors.**

- POWL, *Partially Ordered Workflow Language* (BPM 2023) and *Inductive Mining with
  Choice Graphs* (BPM 2025) — the block-structure limit of Inductive Miner is
  closed; discovery now emits sound-by-construction partially-ordered models.
- TraceCompiler (arXiv 2608.02680, Aug 2026) — an edge is admitted only when the
  consumer argument holds a value **uniquely attributable to an earlier producer**:
  **0.928 precision / 0.943 recall** over 15,775 def-use edges, against **0.711 F1
  for adjacency** and 0.712 for frequency-thresholded directly-follows. Ambiguous
  edges are marked *suspected* and impose no order. It **refuses to compile** when
  an irreversible side effect is under-determined.
- Inducing Task Models from Computer-Use Traces (arXiv 2608.20319) — 0.974
  agreement recovering interleaved concurrent tasks, 74.9% of execution steps
  reconstructed, +30.0% held-out task accuracy over the strongest baseline.
- **OpenAdapt** (production OSS, v1.16.0) — the shipping form of the same thesis:
  *"one demonstration is evidence, not a specification"*. `induce` requires at
  least two recordings and emits a parameterised program **or exits nonzero with no
  bundle**, halting rather than act on unverifiable identity or write.
- AgentLTL (arXiv 2607.02599) — FO-LTL procedural rules over traces yielding a
  deterministic **judge-free** compliance score, usable as an online pre-execution
  gate; +38 / +17.5 pp on held-out patterns including unseen tool names.
- HybridSimilo (EMSE 2026, arXiv 2505.16424) — locates **98.8%** of elements with
  broken locators, on a benchmark 12× Similo's and 23× VON Similo's. Structural
  beats purely visual.
- **A 90-replay fault study through a real persistence boundary** — screen
  verification silently mishandles **5 of 7 transactional fault classes**:
  duplicate submission, optimistic-UI phantom success, partial save, stale
  overwrite, double-click. In each case the recorded pixels match perfectly *and
  the screen shows success*.

**Solved.** Producing a sound workflow graph from a log — soundness is now a
construction guarantee, not a post-hoc check. The alpha-algorithm class of
problems. Variant handling by exhaustive enumeration, replaced by mining the
constraints all paths satisfy. Recovering a broken *locator* on a changed page.

**Open.** There is **no measured quality metric for recovered invariants on UI
traces specifically** — business-process mining has fitness and precision
machinery; nobody has published precision/recall for "the invariants a recorded
enterprise web task must satisfy". The induction objective is **unfalsifiable as
stated**: fitness, precision, generalization and simplicity cannot be jointly
maximized, so every 2026 system falls back to leave-one-out replay, which is a
*consistency* check on the same distribution, not a correctness check. And
essential-versus-incidental is **not recoverable from one trace, by proof** — "one
frame cannot tell you which text is invariant", because clocks and counters look
like dates of birth.

**What it changes.** This is the section that should reorder the roadmap.

- **The five-in-seven result invalidates screen-based verification as the final
  evidence.** Render-drift self-healing cannot touch a duplicate submission or an
  optimistic-UI phantom success, because the screen is *correct* in those cases.
  The `Execute` postcondition must be read from the **system of record** with a
  three-valued CONFIRMED / REFUTED / INDETERMINATE verdict. The UI tells the bot
  what to do; it cannot tell the bot what happened.
- **Induction is a `Query` over an `Observe` corpus, never a compile of one
  recording.** An edge whose producer→consumer attribution cannot be proven is
  carried as unordered and suspected, never guessed by frequency — 0.711 F1 is not
  a fallback for 0.928 precision.
- **Refusal is a first-class `Execute` outcome, and under-determination is the
  common case, not an error path.** OpenAdapt's `induce` exiting nonzero with no
  bundle is the right behaviour and the correct precedent to copy.
- **Essential-versus-incidental is a `Query` question, not a mining question** — a
  step is essential if and only if eliding it changes an externally observable
  effect. That is decidable against declared effects; it is not decidable from the
  trace.
- **Coverage, not accuracy, is the binding constraint.** OpenAdapt's identity gate
  arms only 4–7 of 12 clicks in its own live bundles. Disclosed is not closed.

---

## 5. Workflow verification — the validator I specified implements one of three known properties

**Frontier claim.** Control-flow static validation is a closed, citable discipline:
the workflow-net soundness triple is settled, decidable, and checkable on
realistic models in under 500 ms with automatic repair. The live frontier moved to
SMT/SAT verification of *data-aware* constraints and to static verification of
*agent* workflow graphs — which is what this design is.

**Anchors.**

- van der Aalst et al., *Soundness of workflow nets* (Formal Aspects of Computing
  2011) — the canonical taxonomy: **option to complete**, **proper completion**,
  **no dead transitions**, with a decidability map for every relaxation. Soundness
  for nets *with reset arcs* is proved **undecidable** — the standard example of
  where static checking stops being possible at all.
- Pesic & van der Aalst, *DECLARE* (EDOC 2007) — the canonical declarative
  constraint language: a fixed LTL template set (existence, absence, response,
  precedence, chain-response) that *is* the established way to state cross-step
  constraints.
- De Giacomo et al. (TOSEM 2022) — finite-trace semantics (LTLf/LDLf), the correct
  semantics for a conversation that terminates rather than an infinite trace.
- Maggi et al., *Runtime Verification of LTL-Based Declarative Process Models*
  (RV 2012) — compile each template to an automaton and monitor the live event
  stream instead of exploring a state space.
- Burattin et al. — MP-Declare: activation and target conditions bound to data
  variables, so a rule spans an arbitrary pair of steps, enforced over a joined
  execution model rather than per-screen.
- AgentProof (arXiv 2603.20356, 2026) — the agent-workflow analog, and the closest
  published system to the flow document: structural checks (reachability, dead-end
  detection, tool-declaration coverage, router-edge validation, entry/exit
  structure) plus an LTL DSL compiled to DFA monitors.

**Solved.** The soundness set is settled and decidable; the constraint language is
not in dispute; the semantics for a terminating conversation is settled.

**Open.** No published **handoff criterion** for when static verification should
yield to runtime monitoring — the stated reason is a *semantics gap* (the model's
language is not the implementation's language), which is qualitative, not a
threshold. Data-aware verification does not scale generally: multi-perspective
conformance over unbounded data is undecidable, and every practical system quietly
bounds the data domain. Agent-workflow verification has **no soundness theorem
yet** — pre-consensus, Python-only, validated by example.

**What it changes.**

- **Name the soundness triple in the flow validator.** "Refused on unreachable
  nodes" *is* no-dead-transitions. The two the spec does not yet name are the ones
  that catch a malformed flow: **option-to-complete** (every reachable state can
  still reach an end) and **proper-completion** (the end state is the *only*
  reachable terminal state — a flow able to reach `End` while another branch is
  live violates it). Add both as named checks the invariant register can cite.
- **A closed predicate language over scalars cannot express the register's modal
  cases.** "Must never happen" and "must eventually happen" are temporal, not
  node-local. They belong in the Declare template set with finite-trace semantics,
  compiled one-DFA-per-invariant and run as a **monitor** alongside the verbs.
- **The register accepts exactly two enforcement kinds** — `static-check` and
  `monitor` — and refuses any invariant whose enforcement is review or intent.
  It stays decidable precisely as long as the predicate language stays closed over
  scalars with a **bounded domain**; the moment an invariant quantifies over
  unbounded runtime values, no gate can refuse it and the register degrades to
  documentation.
- **Do not model-check the runtime.** The semantics gap is the argument for the
  monitor seam, not a defect to close.

---

## 6. Capability authority — the bet is supported, and it is one argument check short

**Frontier claim.** Capability-scoped authority for agent tool calls is a real
research field, and the frontier has moved past "hold a capability" to
*invocation-scoped, argument-bound, attested* authority. The decisive negative
result: every major agent framework ships capability gating by default and **none
ships a fail-closed per-call authorization decision**.

**Anchors.**

- ScopeGate (arXiv 2606.28679, Jun 2026) — audit of LangChain/LangGraph,
  LlamaIndex and Stripe Agent Toolkit: all three provide capability gating by
  default; **none provides a deterministic fail-closed per-call value
  authorization gate**.
- ChainCaps (ICML 2026 workshop) — budgets propagate by **intersection**; names
  "permission laundering" — an agent can pass every per-tool check and still
  produce an unsafe end-to-end effect. Cuts attack success from 25–68% to 0–4.8%.
- MiniScope (Dec 2025) — least-privilege enforcement at 1–6% latency overhead.
- Proof-Carrying Agent Actions (Jun 2026) — an action certificate with five
  checkpoints and explicit approval enforceability classes.
- ACLE-MCP (arXiv 2609.02690) — short-lived sender-constrained **leases** binding
  expected workload, freshness, operation, object and parameter bounds, consumed
  at a provider-side execution gate.
- **Ghost Field Exfiltration (SBSeg 2026)** — no prompt injection required: a page
  renders a small visible form and hides extra inputs in the DOM, and Claude for
  Chrome fills them with user data at **19% (Opus 4.6) to 64% (Haiku 4.5)** full
  exfiltration.
- Classical spine: Proof-Carrying Authorization (Bauer/Appel), Macaroons (NDSS
  2014) for attenuation by contextual caveat, Redell & Fabry (TSE 1979) for
  revocation.

**Solved.** Per-tool permission checks are insufficient — permission laundering is
now a named, measured failure mode. Attenuation must be monotone: authority may
lose but never gain through composition. "The framework exposes a tool" is not
authorization, with an audit proving it across the three dominant frameworks.

**Open.** Argument-bound, fail-closed authorization at dispatch — nobody has
shipped a general per-call value-level policy decision point. Revocation: the
agent-era answer is to *avoid granting* rather than to revoke; no system has a
"revoke now, the in-flight action stops" primitive. And **bounding a UI element
rather than an API operation** — a DOM field is not an addressable capability in
any published system, and model-layer defenses fail because they key on
sensitive-data category recognition rather than structural detection of abuse.

**What it changes.**

- **`Auth` proves a grant exists, not that this call's arguments are authorized.**
  `execute_action` needs a ScopeGate-shaped, argument-bound, **default-deny**
  decision at dispatch, in addition to the capability. This is the decisive
  near-term gap.
- **`GrantSet` combination must be intersection-only at the type level.** If two
  weaker `Auth`s can ever union into a stronger one, permission laundering is
  reintroduced above the type system.
- **Answer "granted yesterday, action today" with leases, not revocation.** Give
  `Auth` an expiry and an invocation-scoped binding to operation, object and
  parameter bounds, and consume it linearly rather than holding it.
- **Treat the DOM as a sink.** If `poll`/`query` can read a page into context and
  `execute_action` can write a value into a field, **the field's identity belongs
  in the authorization object** — because Ghost Field Exfiltration proves the
  model will not catch a hidden input, and the substrate is the only layer that
  can. This is the clearest genuinely-unmapped opening in the whole sweep, and it
  is exactly the "point the bot at four or five things and declare invariants"
  idea, made enforceable.

---

## 7. Mixed-initiative and handoff — the initiative question is retired; three criteria replaced it

**Frontier claim.** "Who should act next?" has been replaced by three
decision-theoretic criteria that are each directly implementable: ask when the
expected value of the answer exceeds the cost of asking, abstain when the top
posterior falls below a rejection threshold, and hand off when the human's
expected cost beats the system's.

**Anchors.**

- Horvitz (CHI 1999) — the classical framing: design for uncertainty in both the
  user's intent and the system's ability. Descriptive, not computable.
- Amershi et al., *Guidelines for Human-AI Interaction* (CHI 2019) — 18
  evidence-based guidelines indexed by *when* they apply. **G10 "Scope services
  when in doubt"** and **G9 "Support efficient correction"** make scoping-out and
  redirection design primitives rather than error states.
- Chow (IEEE Trans. Inf. Theory 1970), extended by Mozannar & Sontag (NeurIPS
  2020) — the abstention and **learning-to-defer** criterion, made trainable and
  consistent.
- Rao & Daumé III (ACL 2018) — clarification-question ranking by **expected value
  of perfect information**.
- Bohus & Rudnicky (2008) and the spoken-dialogue error-recovery line — a
  non-understanding re-prompt must **escalate in form** while the attempt count
  stays bounded.
- XSTest (2024) and OR-Bench (2024) — refusal is now benchmarked on **both** axes:
  under-refusal *and* over-refusal, i.e. refusal has a measured false-positive rate.

**Solved.** The handoff criterion exists and is named. Re-asking on
non-understanding is the settled finding, not a local heuristic — and the re-ask
must escalate in form. Refusal is a studied terminal outcome with two-sided
quality criteria. The human-drives-the-UI case is the dominant framing, not a
variant.

**Open.** Calibrated deferral under *real* expert cost, unavailable when the human
is not yet in the loop. Over-refusal has benchmarks but no principled operating
point. No accepted criterion for *which* repair form to escalate to on the n-th
non-understanding. And nothing composes value-of-information, deferral and
rejection into one decision rule over {act, ask, refuse, hand off}.

**What it changes.**

- **Make the resolver return a posterior plus a margin**, and let the runner apply
  a three-way rule: above the accept threshold, advance; ambiguous, **re-ask the
  same node**; below the abstain threshold, terminate.
- **`Refused` must be reachable from a scope predicate evaluated before
  resolution**, and `HandedOff` from a per-node declared human-cost comparison —
  so neither is a catch-all for the step-budget branch.
- **The re-ask counter is per-node, bounded, and escalating in form**: verbatim
  repeat, then narrowed options or explicit confirmation of the top hypothesis,
  then terminal. Recovery falls off after roughly the second repair, and an
  unbounded re-ask is the hostile loop the design already warns about.
- **A two-sided refusal metric belongs in the session evidence** — false-refuse
  rate as well as false-complete rate, since over-refusal is a measured frontier
  defect and proof-carrying authority has no other way to detect it.
- **The session must expose the current node and the reason for the last
  transition**, or the mode-confusion failure is reproduced by design.

---

## 8. Tamper-evident logs and provenance — the minimum is small, and one detail is critical here

**Frontier claim.** Applying transparency-log ideas to AI agent action records is
now the most crowded standards area in agent infrastructure — at least nine IETF
drafts, two evaluated systems, an ISO standard, and a formal systematization — all
converging on signed hash-linked records with an external non-equivocation
commitment. Nothing is deployed at production scale, and the record schema is
contested, not settled.

**Anchors.**

- RFC 6962 / RFC 9162 (Certificate Transparency) — the only production
  transparency log. Its split-view defense, gossip, **never became an RFC** —
  which is the honest statement of what CT actually guarantees.
- RFC 9943 (SCITT architecture, 2026) — generalizes transparency services with
  receipts; the substrate every agent-audit profile now builds on.
- `draft-emirdag-scitt-ai-agent-execution-00` (Apr 2026) — AgentInteractionRecord
  as a COSE_Sign1 payload with hash chain, sequence numbers, identity-binding
  levels, a **redaction receipt**, and compliance mappings to EU AI Act Arts. 12
  and 19.
- Agent Flight Recorder (IEEE BCCA 2026, arXiv 2609.01931) — the first *evaluated*
  agent audit system: eight fields, deterministic CBOR, **48 µs/event**, 100%
  detection of edit/delete/reorder/fork at zero false positives.
- `draft-sahu-agent-action-receipts-00` (Aug 2026) — the tightest statement of the
  minimum: attributable, tamper-evident *as a sequence*, verifiable offline as a
  pure function of records plus an out-of-band trust anchor.
- Cryptographic Erasure on Public Ledgers (IACR ePrint 2026/1109) — systematizes
  retention-versus-immutability, and finds the field over-invested in
  chameleon-hash chain rewriting.
- **EU AI Act Art. 12** (Reg. 2024/1689) — high-risk systems "shall technically
  allow for the automatic recording of events over the lifetime of the system".
  Application 2 Dec 2027 (Annex III) / 2 Aug 2028 (Annex I), paired with Art. 19
  retention.

**Solved.** The cryptographic core. Per-record signature; digest of the
predecessor's **exact transmitted octets**; monotonic sequence number; Merkle
batching; an epoch root published where the operator cannot fork it.
Crypto-shredding is solved as a technique — destroy the key, leave the bytes.

**Open.** The record schema is unsettled — a vendor choosing today is choosing a
draft. There is no *deployed* third party: no production transparency log for
agent actions, no equivalent of CT's monitors. Completeness against a hostile
recorder is undetectable by construction, and a **bounded log makes this worse**.
Retention under conflicting regimes, and the post-compromise boundary as Schneier
and Kelsey drew it in 1998, remain.

**What it changes.**

- **Chain each journal entry over the exact transmitted bytes of its predecessor**,
  including fields a verifier does not recognize. This makes verification
  independent of canonicalization agreement — which matters specifically here,
  because a **shared byte-identical BLAKE3-256 identifier across four repositories
  is exactly the convention a re-canonicalizing digest would silently diverge on**.
  Add a monotonic sequence number; without it neither the bound nor an omission is
  provable.
- **Split authority across two seams, because a hash chain does not detect
  equivocation.** The in-process journal stays authoritative for the fold; a
  separately published epoch root becomes authoritative for non-equivocation. The
  bounded-append refusal is the right backpressure, but **the last-accepted
  sequence number and root must be committed outside the journal**, or the ceiling
  becomes a truncation attack rather than a bound.
- **Never fold over a fact you must forget.** Record the fact shape plus a 32-byte
  commitment; keep the payload in a separately-keyed store with per-event derived
  keys, so retention is met by key destruction while replay still folds. Adopt
  redaction-receipt semantics — record that a field *was* redacted and preserve
  its hash — so replay distinguishes "redacted" from "never present". **Reject
  chameleon-hash chain rewriting**: it mutates a committed leaf and would break the
  shared identifier convention.
- **The two-clock model maps onto the record.** The reducer's decision with its
  policy version is a *recorded* field; the revocation fence's freshness must
  **never enter the fold** — a folded freshness is precisely the yesterday's-
  evidence failure the design exists to prevent. Carry the fence generation as
  provenance beside the policy version.
- **Art. 12 is a commercial lever, not a compliance chore.** Automatic event
  recording for high-risk systems becomes mandatory in 2027–2028. A bot that
  already produces a verifiable action record is selling into that requirement
  rather than retrofitting for it.

---

## 9. Uncertainty and abstention — the four-valued verdict is not over-engineering, and abstention is not a free move

**Frontier claim.** "Can we get a confidence number" is solved — calibration and
conformal prediction both work and ship. The frontier moved to two harder places:
keeping the guarantee valid under the drift the system actually experiences, and
the newly-measured result that **abstention reshapes the human's error rather than
restoring their baseline**.

**Anchors.**

- **Bauer, Leucker & Schallhart** (J. Logic and Computation 2010) — defines a
  four-valued runtime-verification logic whose values are *satisfied / violated /
  presumably violated / presumably satisfied*, refining LTL₃'s three-valued
  good-prefix / bad-prefix / inconclusive. This is the direct formal antecedent for
  a four-valued verdict, and it is derived from axioms, not invented. Their earlier
  **monitorability** result is the load-bearing one: `G(r → F a)`-shaped properties
  have **no good or bad finite prefix**, so they are permanently inconclusive —
  some constraints cannot be decided from a finite observation, ever.
- **Belnap** (Artificial Intelligence 1998) — the other antecedent: truth and
  falsity as *independent* evidence, giving {true, false, both, neither}. That is
  the right algebra when corroborating and refuting evidence arrive from separate
  detectors, which is exactly the DOM-heuristic case.
- Guo et al. (ICML 2017) and *Revisiting the Calibration of Modern Neural Networks*
  (NeurIPS 2021) — modern nets are systematically overconfident; temperature
  scaling fixes it in-distribution and **does not hold out-of-distribution**. Raw
  model confidence is not a usable abstention signal.
- Angelopoulos & Bates (2023) and **Gibbs & Candès** (Annals of Statistics 2023) —
  conformal gives distribution-free finite-sample coverage; adaptive conformal
  restores a **long-run** coverage guarantee under arbitrary drift, with
  step-to-step coverage fluctuating. It does not guarantee the *next* decision.
- **Jabbour et al.** (arXiv 2508.07617, N=259 clinicians) — the decisive abstention
  study. No AI: 66% accuracy. Inaccurate AI: 56%. Selective prediction: 64%. But
  abstention **shifted the error distribution**: +18% missed diagnoses and +35%
  missed treatments versus *no AI at all*. The assumption that "when the AI
  abstains, the human decides as if no AI had spoken" is **empirically false**.
- Measured failure taxonomies, converging — CUADebug (arXiv 2608.02643: 204 failed
  OSWorld trajectories, human-annotated root cause; structured re-execution lifts
  recovery **13.9% → 29.9%**) and *Why Do LLM-based Web Agents Fail?* (ACL 2026).
  Across gpt-5-nano, claude-haiku-4.5 and gemini-flash-2.5, **low-level execution —
  perceptual grounding and control — is the dominant bottleneck**, not high-level
  planning.

**Solved.** Confidence *can* be made honest. The cost question is answered:
conformal's marginal runtime cost is one nonconformity score plus a quantile
lookup, negligible beside a model call — **the real cost is data, not compute**,
because you need an exchangeable labelled calibration set *of the exact decision
you are gating*. The failure taxonomy is measured, not theorised.

**Open.** There is **no published A/B deployment** showing conformal-gated
abstention improves end-to-end reliability of an agent acting on a live UI — which
is precisely why the "thousands of lines of DOM heuristics that pass fixtures and
miss the real element" failure mode remains undefended. Aleatoric-versus-epistemic
is theoretically clean and practically unstable; do not build on the label, build
on computable proxies. **Coverage is not correctness** — if the prediction set is
large the coverage guarantee is worthless for acting, so **set size, not coverage,
is the operational quantity**, and controlling set size under drift is open. And
no head-to-head study compares a four-valued run verdict against two-valued
pass/fail on a long-horizon benchmark: the formalism is well-founded, the empirical
claim that it catches more real failures is untested.

**What it changes.**

- **The four-valued verdict stands, and it is not over-engineering.** Verified /
  failed / unverified / fatal map onto good prefix / bad prefix / inconclusive /
  monitor-abort, and the two "presumably" cases are the two sides of the
  uncertainty an unverified verdict must carry. Grounded in RV-LTL and Belnap, not
  invented here.
- **Enforce the run rule as a lattice meet over step verdicts** (Kleene
  conjunction) rather than a special case, so partial unverification propagates to
  the run verdict by construction.
- **The invariant DSL must reject non-monitorable constraints at declaration
  time.** A `G(r → F a)`-shaped property has no good or bad finite prefix and can
  only ever be unverified; either forbid it or mark it permanently inconclusive.
  This is a *load-time* check, which is exactly what the register's refusal
  machinery is for.
- **Escalation must not hand the task back as if the bot were absent.** Jabbour et
  al. shows that path produces a measurably worse error profile, so the escalation
  payload carries the partial evidence and the named unverified constraint. This
  changes `HandedOff` from a bare target into a payload.
- **Use one nonconformity score for three jobs** — abstention, drift detection, and
  deciding to re-observe — and apply conformal risk control at the **trajectory**
  level, reserving per-click conformal for replayed traces where a per-click
  calibration set actually exists.
- **This closes the loop with section 4.** The measured dominant failure is
  grounding and control, not knowledge — so an uncertainty model that only
  represents what the bot *knows* misses most of the failure mass. The uncertainty
  has to be about **acquisition**, which is the three-way resolver verdict from
  section 1.

---

## The synthesis: what the bot's differentiator actually is

Every individual capability above is contested, patented, or already shipped by
someone. That is the finding, and it rules out the obvious strategy of picking one
and claiming it.

What no commercial product and no research system has is **the conjunction**:

| Layer | The piece no one else has |
|---|---|
| Observation | `ElementRef` minted **only** from a resolved three-way verdict, so a healed element has no type-level path into execution |
| Effect | The `Execute` postcondition is read from the **system of record**, not the screen — because in five of seven silent-write fault classes the screen is *correct* |
| Authority | Argument-bound, default-deny dispatch on top of a sole-constructor grant, with intersection-only composition and leases instead of revocation |
| Sink bounding | The **DOM field as an addressable authorization object** — the one gap the sweep found genuinely unmapped |
| Flow | A validator bound to the named soundness triple, with modal invariants monitored rather than asserted |
| Evidence | A journal verifiable by a third party against exact transmitted bytes, with an epoch root outside it |
| Regression | A drift-mutation harness, because the entire field lacks one |

Three of these are supported by measured evidence as *the* correct design rather
than a preference: the three-way observation verdict (the distributions provably
overlap), the cascade with no model on the default path (88% at zero tokens,
reranked to ~95%), and the argument-bound authority gate (permission laundering is
measured, and no framework ships the gate).

Two are bets, and should be recorded as bets: that a DOM field can be modelled as
an addressable capability, and that a drift harness is worth more than a
leaderboard position.

One is a correction the project owes itself: the flow validator implements one of
three known soundness properties, and the invariant register needs a bounded data
domain to remain decidable at all.
