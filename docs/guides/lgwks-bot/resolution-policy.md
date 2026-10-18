# Resolution: thresholds, validation and logging

Pages of this guide: [verdicts](resolution.md) · [the two readings and the lexical tier](resolution-readings.md) · [aliases, the semantic tier and the receipt](resolution-aliases.md) · [policy and logging](resolution-policy.md)

## Thresholds are declared, not fitted

`SemanticPolicy::DEFAULT` is `{ threshold: 0.72, margin: 0.05 }`
(`crates/lgwks-bot/src/semantic.rs:356`). Those numbers are constructor
arguments, not tuned constants, because the right values depend on the model.

`docs/general-bot-fold-frontier.md` §6 item 13 records what is missing, and it is worth
quoting rather than paraphrasing: there is no way to *find* them or to *measure*
whether they are right, and "until this exists, 'the semantic tier helps' is an
assertion with no measurement behind it, and step 3's constants are unexamined."

So: do not treat these defaults as calibrated for your domain. There is no
held-out result, no abstention study, and no per-domain evaluation in the
inspected source. Choosing a threshold for your data is your evaluation to run.
The provider seam and its limits are documented; the quality claim is not
available.

## Representation, decision rule, validation

Three things are easy to conflate, and the code keeps them apart.

- **Representation.** `Resolution::Resolved { tier, score, lead }` records that a
  resolver selected an outcome according to its own rules. It does not certify
  that the selected element is the one the person meant. A `Resolved` verdict is
  a decision, not a correctness proof. In particular, `Exact` means the two
  strings are equal under a *declared* normalization policy — it is not a claim
  that the transformation preserved everything the input meant, which is why a
  reading whose policy would erase something meaning-bearing does not use the
  policy at all.
- **Decision rule.** `SemanticPolicy` and the two `MATCH_*` constants are the
  rule. Their values are policy, and policy is not measurement.
- **Validation.** `FlowSpec::validate` proves the graph is well-formed, and it
  now has something to say about the options themselves: two options that the
  declared policy cannot tell apart — the same normalized label, or the same
  decoded value — are refused at construction, naming both options. A question
  that offers them could never report which one the person chose, and the
  failure would surface as a tie at runtime rather than as an error in the flow.
  A signed integer question offering `-5` and `5` is accepted, because under its
  policy those are two values; the same two strings offered as labels are not.

An option list that fits no phrasing will produce `Absent` forever, and no
resolver change fixes an option list that does not contain the answer.

## What to log

`Degraded` exists so an operator can tell a person who was unclear from a
dependency that is down. The session records the degraded re-ask under its own
transcript role (`crates/lgwks-bot/src/session/runner.rs:659`), and a superseded
binding under a different one (`crates/lgwks-bot/src/session/runner.rs:673`), so the
three do not render identically. If you are building a dashboard, the degraded
role is the signal to alert on; `Absent` is not. The superseded-binding role is
neither: it is a prompt for someone to ask the person what they meant, and it
will repeat exactly as often as the phrase is used until the flow's options or
the table are reconciled.
