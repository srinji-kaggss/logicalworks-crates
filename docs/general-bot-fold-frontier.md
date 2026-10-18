# Folding the recorder: the sequence, steps 6 to 14

Pages of this guide: [scope and finding](general-bot-fold.md) · [what is new](general-bot-fold-new.md) · [recorder, settlement, language](general-bot-fold-language.md) · [semantic tier and mapping](general-bot-fold-semantic.md) · [reaching a page](general-bot-fold-reach.md) · [evasion and injection](general-bot-fold-evasion.md) · [sequence 1 to 5](general-bot-fold-sequence.md) · [sequence 6 to 14](general-bot-fold-frontier.md)

6. **The politeness frontier.** ✅ Landed in this change: `frontier.rs` —
   `ConstraintKey`, `Resolved`, `RulesState`, the three-armed `Admission`, a
   validated `PolitenessPolicy`, and the `Frontier` that admits against every
   constraint a request shares. Three of this document's own commitments were
   revised while implementing it, and each revision is a finding rather than a
   preference:

   - **The verdict is three-armed, not four, and §5.3's three are right.** The
     first draft promoted each cause to its own arm. That is wrong for a reason
     the cause-list makes obvious once written out: upstream failure mechanics
     are unbounded — a DNS `SERVFAIL`, a TLS timeout, and a redirect loop on
     `/robots.txt` are all *we could not look*, and none changes what the
     scheduler does. The epistemics belongs in `RulesState` and `Resolved`,
     which are explicit state machines, and the verdict reports only the
     operational result. The property that makes that safe, and the bug it
     avoids: **nothing provisional may be spelled `Reject`**. A terminal drop and
     a re-queue are different actions, and a consumer that must inspect a payload
     to learn which one it holds has no verdict.
   - **RFC 9309 supplies a third arrival of the invariant, and RFC 8020 a
     fourth.** *No rules were retrieved* has two opposite answers decided by
     why: §2.3.1.3's 4xx means unavailable and "the crawler MAY access any
     resources", while §2.3.1.4's 5xx means unreachable and the crawler "MUST
     assume complete disallow" — and only *until a fresh, valid file is
     obtained*, which is why `Unreachable` carries an instant and expires.
     Resolution splits the same way: `NXDOMAIN` is authoritative and rejects
     (RFC 8020), while `SERVFAIL` and timeouts are not and defer.
   - **Politeness is keyed on infrastructure, not hostnames, and the delay was
     the wrong control variable.** A per-host limit is blind to what a host is:
     two hundred Cloudflare-fronted domains are two hundred "independent" polite
     queues that are one reverse proxy seeing two hundred requests a second from
     one egress — a Layer 7 flood with a polite-looking config. So a request is
     admitted only when every constraint it shares admits. And the controlled
     quantity is an **integer concurrency window**, not a delay derived from
     latency: Scrapy's `AutoThrottle` makes `target_concurrency` a divisor rather
     than a bound, holds instead of backing off on a non-200, never reads
     `Retry-After`, and reads latency as load when a CDN edge cache answering
     fast makes it *speed up* under origin strain.

   Not yet here, and named rather than implied: the *drivers* that fetch rules
   and resolve names — this module models both states and records the
   observations, but nothing performs them — and §8.1's durable `Pending`/`Done`
   frontier, whose two-phase mark matters as soon as a run can be killed
   mid-flight.
7. **Declared identity**, wired as a flow property and recorded per run (§5.2).
8. **The `bot.evade` capability**, with the seeded, recorded draw and the grant
   log (§5.4, §5.5). Deliberately after 6 and 7, so the cheap remedies are the
   ones in place before the expensive one is reachable.
9. **The settlement policy.** `Time<Virtual>`-driven, declared per step, with
   non-settlement as a typed terminal outcome (§3.4).
10. **The recorder and its visible-delta compiler** (§3.3), on the ECS substrate.
11. **The agent domain**, closed-enum actions and `FlowBounds` budget (§5.7).
12. **The MCP adapter**, over the four verbs.
13. **Semantic-tier calibration.** `SemanticPolicy::DEFAULT` is `{ threshold:
    0.72, margin: 0.05 }`: declared, not fitted (§3.6), and constructor
    arguments because the right values depend on the model. What is missing is
    any way to *find* them or to *measure* whether they are right. Semantic
    Router — the closest production implementation — ships `fit(X, y)`,
    `evaluate(X, y)` and `get_thresholds()` for exactly this. The useful part of
    that precedent is its own published result: a five-route healthcare split
    scores **54.2%** validation accuracy *after* fitting, so the honest number
    for embedding-similarity routing can be far worse than a threshold's
    confidence suggests. Until this exists, "the semantic tier helps" is an
    assertion with no measurement behind it, and step 3's constants are
    unexamined.
14. **Precomputed embeddings at the resolver boundary.** `SemanticResolver`
    embeds the utterance and every candidate option on each `resolve`, so a flow
    with *n* options costs *n + 1* provider calls per turn, every turn. The same
    implementation's `BaseRouter.__call__` takes an optional `vector` that skips
    encoding entirely and pushes the lifetime decision to the caller. That shape
    is what fits here: it needs no cache, which the estate forbids without a
    bound and an eviction policy, and it keeps one implementation of the
    decision rather than adding a second entry point to the same work.

One note against a future simplification. Semantic Router's `BaseRouter`
*raises* when its index is not built — the same *could not look* situation
`Resolution::Degraded` carries as a verdict. Both express it; only one forces
the caller to handle it. It is recorded here because `Degraded` looks like a
value that could be folded into `Absent`, and it cannot: that is the exact
collapse step 5 exists to prevent.

## 7. What this document does not decide

- Whether the durable queue is a storefront feature or stays out of the
  workspace entirely. It needs the dependency doctrine's ruling, not this
  document's.
- The scheduler decision in `docs/bot-on-ecs.md` §9. It required project-owner
  sign-off and no longer does: taken on 2026-09-21 as a first-party `lgwks_std`
  module, with the finding it was taken against recorded in §9.
- Anything about the described platform's licence. Nothing was copied.
