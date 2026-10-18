# Resolution: the two readings and the lexical tier

Pages of this guide: [verdicts](resolution.md) · [the two readings and the lexical tier](resolution-readings.md) · [aliases, the semantic tier and the receipt](resolution-aliases.md) · [policy and logging](resolution-policy.md)

## The two readings

Which reading an answer gets is decided by the question's `AnswerDomain` before
any matching happens, and the two readings share no tier.

### An `Integer` answer is decoded and compared as a value

The raw utterance goes through the resolver's integer decoder
(`crates/lgwks-bot/src/session/value.rs:207`), which runs the same `parse::<i64>` over
the same trimmed bytes as the store's `VarType::decode_answer`
(`crates/lgwks-bot/src/session/value.rs:151`) — so an answer the resolver can compare
is an answer the variable can hold, and the two cannot disagree about which
answers are whole numbers. It trims surrounding space and accepts a leading
sign, so `" 5 "`, `"+5"` and `"5"` all name the value `5`, while `"5.0"`,
`"five"` and an out-of-range literal such as `9223372036854775808` name nothing.
The decoded value is then compared with each option's own decoded value, and the
ordinary decision rule runs over that set
(`crates/lgwks-bot/src/language.rs:465`).

No lexical, phonetic, fuzzy, alias or semantic tier runs for this domain, and
that is the point rather than an optimisation. `normalize` folds punctuation and
symbols, so `normalize("-5") == normalize("5")`; an answer read as a label would
match `"5"` at the Exact tier with score `1.0` and the sign would be gone. An
alias lookup is keyed on that same folded text and a phonetic key is computed
from it, so both are the same loss through another door — and a model asked which
of two numerals an utterance resembles would answer with the closest-looking one.
`-5` is not a near miss for `5`; it is a different value.

A number no option holds is `Absent`, so the question is re-asked and no route is
taken. Both signs can be offered (`"-5"` and `"5"` are two values), the ends of
the `i64` range round-trip exactly, and an answer that exceeds the range is
refused rather than wrapped or clamped. Nothing is ever parsed from a *selected
option* — the value stored is the value the person gave.

### A `Label` answer goes through the tiers

Everything below is the Label reading.

## The lexical tier

`LanguageResolver` is a tiered match over a lexicon, not a model
(`crates/lgwks-bot/src/language.rs:1`). Each tier is a pure function of the
input, so a verdict is reproducible and a wrong one is explicable by naming the
tier that produced it.

| Tier | Rule | Score |
|---|---|---|
| `Exact` | the normalized utterance equals the normalized option, or is a learned alias for it | 1.0 |
| `Phonetic` | the two collapse to the same sound-alike key | 0.9 |
| `Fuzzy` | weighted blend of token overlap and bounded edit distance | computed |

They are ordered tiers rather than one weighted sum because an exact match has to
short-circuit, and a weighted sum cannot express that. `MatchTier` is reported
rather than inferred (`crates/lgwks-bot/src/session/resolve.rs:226`), because the tier
names the repair: an `Exact` miss is a learned alias pointing at the wrong
option, a `Phonetic` miss is the English bias of the sound-alike key, a `Fuzzy`
miss is a threshold.

**Precedence is over the whole candidate set, not per option**
(`crates/lgwks-bot/src/language.rs:294`). The best tier that any option reached is
the tier the question is answered in, and every option below it is discarded
before the margin is applied. An option does not win by scoring higher than an
option in a stronger tier, and it does not take an `Ambiguous` verdict from a
weaker tier as licence to be selected from a ranking of its own. Without that
rule a fuzzy competitor can outscore a phonetic match on the raw number — a
transposition of two words keeps the whole token set and can score above `0.9` —
and the tier order would be decoration.

The three constants are `MATCH_THRESHOLD = 0.55`
(`crates/lgwks-bot/src/language.rs:85`), `MATCH_MARGIN = 0.08`
(`crates/lgwks-bot/src/language.rs:99`) and `MAX_UTTERANCE_CHARS = 512`
(`crates/lgwks-bot/src/language.rs:76`). A score at or above the threshold that
does not lead the runner-up **in its own tier** by the margin is `Ambiguous`, not
`Resolved`. An utterance longer than the bound degrades rather than panicking.

The threshold and the margin are asked in that order, and the order is the
contract. Every option is measured first; the threshold then says which options
*may win*, and the margin asks whether the winner separated itself from the best
of everything else that was measured — including an option whose score fell just
short of the threshold. A below-threshold option does not stop being a measured
proximity because it may not be selected, so it still counts against the lead
and still appears in `Ambiguous::tied`. Filtering first and measuring second was
the defect GitHub issue #31 filed: it let a competitor one hundredth below the
threshold disappear, so a winner holding a `0.02` lead against a required `0.05`
reported the whole of its score as a margin and was accepted.

The phonetic tier deliberately does not absorb a differing initial consonant, so
`cat`/`kat` and `Catherine`/`Kathryn` do not match. The module documentation
calls that the conservative choice rather than an oversight.
