//! Language understanding: turning what a person actually typed into a choice.
//!
//! The seam is [`crate::session::Resolver`]. This module is its shipped
//! implementation, and it is built the way classic autocomplete and
//! command-and-control speech recognisers are built: a **tiered match over a
//! lexicon**, not a model. Each tier is a pure function of the input, so a
//! resolution is reproducible and a wrong one can be explained by naming the
//! tier that produced it.
//!
//! # The tiers, and why they are tiers rather than a weighted sum
//!
//! 1. **Exact** — the normalized utterance equals the normalized option, or is
//!    a learned alias of it. Score `1.0`.
//! 2. **Phonetic** — the two collapse to the same [`phonetic_key`]. Score `0.9`.
//!    This is the tier that absorbs spelling variation **after the first
//!    letter**: `Smith`/`Smyth`, `there`/`their`, `Straße`/`Strasse`. It
//!    deliberately does *not* absorb a differing initial consonant, so
//!    `cat`/`kat` and `Catherine`/`Kathryn` do not match — see [`phonetic_key`]
//!    for why that is the conservative choice rather than an oversight.
//! 3. **Fuzzy** — a weighted blend of token overlap ([`Jaccard`]) and bounded
//!    edit distance ([`EditDistance`]) over the normalized forms.
//!
//! They are ordered tiers rather than one [`Weighted`] sum because an exact
//! match must *short-circuit*, and a weighted sum cannot express that: a
//! candidate that is perfect on one vector and absent on another scores as
//! mediocre under a sum, and a candidate that is mediocre everywhere scores as
//! comparable. Ordering is the policy, and it is the reason a resolution can
//! report which tier it came from.
//!
//! # Precedence is over the whole candidate set, not per option
//!
//! The order above is applied to the **set**, not to each option: an exact
//! candidate anywhere in the list ends the search, and the phonetic and fuzzy
//! tiers are not consulted at all. Tagging each option with its own tier and
//! then sorting the mixture by numeric score is not a tier order, because the
//! numbers do not mean the same thing in different tiers — a fuzzy candidate
//! can score `0.97` against text an exact candidate matched perfectly, and one
//! margin applied across both ranks reports `Ambiguous` for an answer the
//! person typed in full. Only the winning tier is ever compared, and the margin
//! is applied inside it, which is the only place it has a meaning.
//!
//! [`Weighted`]: lgwks_std::similarity::Weighted
//! [`phonetic_key`]: crate::language::phonetic_key
//! [`Jaccard`]: lgwks_std::similarity::Jaccard
//! [`EditDistance`]: lgwks_std::similarity::EditDistance
//!
//! # Two axes of "linguistic change", and they need different mechanisms
//!
//! A person's phrasing drifts for two unrelated reasons, and one mechanism
//! cannot cover both:
//!
//! - **Spelling and sound.** Typos, transliteration, regional spelling. This is
//!   a *property of the letters*, so it is handled statically by the phonetic
//!   tier and needs no learning.
//! - **Vocabulary.** New slang, a house term, an abbreviation, an inside joke.
//!   `"the usual"` cannot be derived from `"Repeat last order"` by any amount of
//!   string distance. This is a *property of the group's usage*, so it is
//!   handled by [`LanguageResolver::learn`], which records an alias when a
//!   resolution is confirmed.
//!
//! The second is deliberately a **count-free, readable table** rather than a
//! fitted model. Every alias is a row a reviewer can read, and revoking one is
//! deleting a row. A learned scorer would be as adaptive and would not be
//! auditable, and `docs/security-posture.md` rests on the run being declarable.

use crate::session::by_score_descending;
use std::collections::BTreeMap;

use lgwks_std::similarity::{EditDistance, Jaccard, Similarity};

use crate::session::{
    AnswerDomain, MatchTier, PolicyVersion, Provenance, Question, Resolution, Verdict,
    decode_integer,
};

/// The longest normalized input the shipped resolver will compare.
pub const MAX_UTTERANCE_CHARS: usize = 512;

/// The same bound as [`MAX_UTTERANCE_CHARS`], in the width the policy digest
/// carries.
///
/// A second spelling, because `usize` has no infallible `f64` conversion and the
/// narrowing one leaves a failure arm that has to put *something* in the digest —
/// and a receipt that names a bound the resolver is not using is worse than no
/// receipt. `the_digest_input_bound_is_the_shipped_bound` asserts the two agree,
/// so the second spelling cannot drift without a test failing.
const INPUT_BOUND_CHARS: u32 = 512;

/// The token-overlap weight inside the fuzzy tier.
const TOKEN_WEIGHT: f64 = 0.5;

/// The edit-distance weight inside the fuzzy tier.
const DISTANCE_WEIGHT: f64 = 0.5;

/// The score at or above which a fuzzy match is a candidate at all.
pub const MATCH_THRESHOLD: f64 = 0.55;

/// The lead the best candidate must hold over the runner-up *in its own tier*.
pub const MATCH_MARGIN: f64 = 0.08;

/// The tier label this module's policy version is declared under.
const POLICY_LABEL: &str = "lexicon";

/// Folds text to a comparable form: lower case, ASCII, single-spaced.
///
/// The diacritic fold is a **bounded table**, not a general Unicode
/// decomposition, because this crate may not take a Unicode dependency and a
/// hand-rolled approximation should state its ceiling rather than imply
/// completeness. It covers Latin-1 Supplement and Latin Extended-A — the range
/// ordinary European input uses. A character outside it passes through
/// unchanged, which degrades to the fuzzy tier rather than mis-matching.
///
/// Ligatures fold to their **base letter**, not to their expansion: the fold is
/// one character in, one character out, so `œ` becomes `o` and not `oe`, and
/// `ß` becomes `s` and not `ss`. The dropped letter is exactly the kind of
/// one-edit difference the fuzzy tier absorbs, so `Straße`/`Strasse` still
/// resolves; it is recorded here because a reader would otherwise expect an
/// expansion.
#[must_use]
pub fn normalize(text: &str) -> String {
    let mut folded = String::with_capacity(text.len());
    let mut pending_space = false;
    for character in text.chars() {
        let mapped = if character.is_alphanumeric() {
            fold_diacritic(character)
        } else {
            // Punctuation, symbols and whitespace all become a separator, so
            // "yes!" and "yes" and "yes," are one form.
            if folded.is_empty() {
                continue;
            }
            pending_space = true;
            continue;
        };
        if pending_space {
            folded.push(' ');
            pending_space = false;
        }
        for lower in mapped.to_lowercase() {
            folded.push(lower);
        }
    }
    folded
}

/// Maps one accented Latin character to its unaccented ASCII base.
///
/// Returns the character unchanged when it is outside the covered ranges.
fn fold_diacritic(character: char) -> char {
    match character {
        'à'..='å' | 'ā' | 'ă' | 'ą' => 'a',
        'À'..='Å' | 'Ā' | 'Ă' | 'Ą' => 'A',
        'è'..='ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => 'e',
        'È'..='Ë' | 'Ē' | 'Ĕ' | 'Ė' | 'Ę' | 'Ě' => 'E',
        'ì'..='ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' => 'i',
        'Ì'..='Ï' | 'Ĩ' | 'Ī' | 'Ĭ' | 'Į' => 'I',
        'ò'..='ö' | 'ø' | 'ō' | 'ŏ' | 'ő' => 'o',
        'Ò'..='Ö' | 'Ø' | 'Ō' | 'Ŏ' | 'Ő' => 'O',
        'ù'..='ü' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => 'u',
        'Ù'..='Ü' | 'Ū' | 'Ŭ' | 'Ů' | 'Ű' | 'Ų' => 'U',
        'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => 'c',
        'Ç' | 'Ć' | 'Ĉ' | 'Ċ' | 'Č' => 'C',
        'ñ' | 'ń' | 'ņ' | 'ň' => 'n',
        'Ñ' | 'Ń' | 'Ņ' | 'Ň' => 'N',
        'ý' | 'ÿ' | 'ŷ' => 'y',
        'Ý' | 'Ÿ' | 'Ŷ' => 'Y',
        'ß' => 's',
        'ś' | 'ŝ' | 'ş' | 'š' => 's',
        'Ś' | 'Ŝ' | 'Ş' | 'Š' => 'S',
        'ź' | 'ż' | 'ž' => 'z',
        'Ź' | 'Ż' | 'Ž' => 'Z',
        'ĝ' | 'ğ' | 'ġ' | 'ģ' => 'g',
        'Ĝ' | 'Ğ' | 'Ġ' | 'Ģ' => 'G',
        'ł' => 'l',
        'Ł' => 'L',
        'ř' => 'r',
        'Ř' => 'R',
        'ţ' | 'ť' | 'ŧ' => 't',
        'Ţ' | 'Ť' | 'Ŧ' => 'T',
        'ď' | 'đ' => 'd',
        'Ď' | 'Đ' => 'D',
        'æ' => 'a',
        'Æ' => 'A',
        'œ' => 'o',
        'Œ' => 'O',
        other => other,
    }
}

/// Reduces text to a sound-alike key, so spelling variation collapses.
///
/// This is **Soundex**, chosen over Metaphone or Double Metaphone for a
/// property that matters more here than accuracy: it is short, total, and
/// reproducible by hand, so a reviewer can confirm a match without trusting
/// this implementation. The cost is that it is English-biased and loses
/// information on purpose — `there` and `their` collide, which is the point.
///
/// Two empty keys never match: an input with no ASCII letters produces the
/// empty key, and treating two of those as equal would make every unpronounceable
/// pair a phonetic match. An empty key is returned empty, not padded — a padded
/// `"0000"` would be a *non-empty* key, and every digit-only input would then
/// collide with every other.
///
/// **The first letter is kept verbatim, which is deliberate and is a real
/// limitation.** It is why `Smith`/`Smyth` collapse but `Catherine`/`Kathryn` do
/// not: both code `365` after the initial, and the initial is preserved. The
/// alternative variant — coding the first letter too — would match that pair and
/// would also collapse `cat`/`sat`, because `C`, `K` and `S` share a code. In a
/// resolver a false positive is strictly worse than a false negative: a false
/// negative re-asks, and a false positive silently selects the wrong option.
/// The conservatism is the point.
#[must_use]
pub fn phonetic_key(text: &str) -> String {
    let normalized = normalize(text);
    let mut key = String::with_capacity(4);
    let mut previous = '0';
    for character in normalized.chars() {
        if !character.is_ascii_alphabetic() {
            continue;
        }
        let code = soundex_digit(character.to_ascii_uppercase());
        if key.is_empty() {
            // The first letter is kept verbatim, not coded.
            key.push(character.to_ascii_uppercase());
            previous = code;
            continue;
        }
        if code == '0' || code == previous {
            previous = code;
            continue;
        }
        key.push(code);
        previous = code;
        if key.chars().count() >= 4 {
            break;
        }
    }
    if key.is_empty() {
        return key;
    }
    while key.chars().count() < 4 {
        key.push('0');
    }
    key
}

/// Maps one upper-case ASCII letter to its Soundex code.
fn soundex_digit(letter: char) -> char {
    match letter {
        'B' | 'F' | 'P' | 'V' => '1',
        'C' | 'G' | 'J' | 'K' | 'Q' | 'S' | 'X' | 'Z' => '2',
        'D' | 'T' => '3',
        'L' => '4',
        'M' | 'N' => '5',
        'R' => '6',
        // Vowels and H/W/Y are separators, not codes.
        _ => '0',
    }
}

/// Splits normalized text into its tokens.
fn tokens(text: &str) -> Vec<String> {
    normalize(text)
        .split(' ')
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Orders the tiers by precedence: the lowest rank wins outright.
///
/// The order is the module's policy and is stated once, here, so precedence is
/// a property of the resolver rather than of the numbers a tier happens to
/// produce. `Semantic` ranks last because it is consulted only when the lexicon
/// found nothing at all, and never to outvote a lexical verdict.
const fn tier_rank(tier: MatchTier) -> u8 {
    match tier {
        MatchTier::Exact => 0,
        MatchTier::Phonetic => 1,
        MatchTier::Fuzzy => 2,
        MatchTier::Semantic => 3,
    }
}

/// Reduces scored candidates to the highest tier that produced any.
///
/// This is the step that makes "Exact, Phonetic, Fuzzy" an *order* rather than a
/// label. Scoring every option, tagging each with the tier it reached, and then
/// sorting the mixture by numeric score is not a tier order: a fuzzy candidate
/// can score 0.97 against an option an exact answer already matched perfectly,
/// and the margin rule then reports `Ambiguous` for an answer the person typed
/// in full. Cross-tier scores are not comparable — each tier's number means
/// something different — so exactly one tier is allowed into the comparison.
fn winning_tier(mut scored: Vec<(usize, MatchTier, f64)>) -> Vec<(usize, MatchTier, f64)> {
    let Some(best) = scored.iter().map(|candidate| tier_rank(candidate.1)).min() else {
        return scored;
    };
    scored.retain(|candidate| tier_rank(candidate.1) == best);
    scored
}

/// Scores every option against one utterance, best-first, within the winning
/// tier.
///
/// Deterministic: the returned order is by descending score, and ties keep
/// lowest-index-first, so the caller's verdict does not depend on iteration
/// order — the comparator rule `docs/bot-on-ecs.md` §8 takes from Heritrix.
///
/// Every option is measured and every measurement is returned, including one
/// that falls below [`MATCH_THRESHOLD`]. The threshold is not applied here
/// because it is not this function's decision to make: it says which option may
/// *win*, and a winner can only be chosen against a field. Applying it here
/// would delete a competitor before [`decide`] measured the lead over it, which
/// is how a near tie comes to be reported as a decisive win.
fn score_all(
    utterance: &str,
    question: &Question<'_>,
    aliases: &BTreeMap<String, BTreeMap<String, Alias>>,
    distance: &EditDistance,
) -> Vec<(usize, MatchTier, f64)> {
    let options = question.options();
    let spoken = normalize(utterance);
    let spoken_key = phonetic_key(utterance);
    let spoken_tokens = tokens(utterance);
    let overlap = Jaccard::<String>::new();
    // The confirmed option this phrase names *in this question*, if any. An
    // index is never consulted: the binding is resolved to a position in the
    // list in front of the person right now, and a binding whose option is no
    // longer here contributes nothing rather than degrading into a re-binding.
    let bound = aliases
        .get(question.id())
        .and_then(|table| table.get(&spoken))
        .map(Alias::option);

    let mut scored: Vec<(usize, MatchTier, f64)> = Vec::with_capacity(options.len());
    for (index, option) in options.iter().enumerate() {
        let canonical = normalize(option);
        let tier_and_score = if bound == Some(option.as_str()) {
            // A learned alias is an exact match: a person confirmed it.
            Some((MatchTier::Exact, 1.0))
        } else if spoken == canonical {
            Some((MatchTier::Exact, 1.0))
        } else if !spoken_key.is_empty() && spoken_key == phonetic_key(option) {
            Some((MatchTier::Phonetic, 0.9))
        } else {
            let lexical = distance.score(&spoken, &canonical);
            let set = overlap.score(&spoken_tokens, &tokens(option));
            // Kept whatever it measures. A blend below the threshold is a
            // measured proximity that `decide` still has to see.
            Some((
                MatchTier::Fuzzy,
                (TOKEN_WEIGHT * set) + (DISTANCE_WEIGHT * lexical),
            ))
        };
        if let Some((tier, score)) = tier_and_score {
            scored.push((index, tier, score));
        }
    }

    let mut scored = winning_tier(scored);
    // Best-first through the crate's one comparator, so the lexicon and the
    // semantic tier cannot order the same two candidates differently. `sort_by`
    // is stable and the input is in ascending index order, so a tie keeps
    // lowest-index-first.
    scored.sort_by(by_score_descending);
    scored
}

/// One confirmed binding: *in this question, this phrase means this option*.
///
/// The three fields are the three things a confirmation actually asserts, and
/// leaving any of them out is how the confirmation becomes a lie:
///
/// - `question` is the vocabulary scope. `"the usual"` means `"Repeat last
///   order"` *at the ask that offered it*; the same phrase at a later question
///   means nothing in particular, and must not be spent there.
/// - `utterance` is the phrase, normalized once at construction so the table has
///   exactly one spelling of it.
/// - `option` is the **stable option identity**: the option's own text, verbatim.
///   Not an index, because an index is a position and positions are reused —
///   teaching row 0 and then replacing row 0's text would silently transfer the
///   confirmation to whatever moved in. Not a normalized form either, because
///   normalization is lossy in exactly the way that matters here (`-5` folds to
///   `5`; see #38), and an identity that aliases two distinct options is not an
///   identity.
///
/// The identity is resolved into the current option list at use time, so an
/// option that moves to another position keeps its confirmation and one that is
/// removed does not pass it to its successor.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Alias {
    /// The question whose vocabulary the confirmation was made in.
    question: String,
    /// The confirmed phrase, normalized.
    utterance: String,
    /// The option's own text, verbatim: the stable identity.
    option: String,
}

impl Alias {
    /// Binds `utterance` to `option` within `question`'s vocabulary.
    ///
    /// The utterance is normalized here rather than at insertion so that every
    /// `Alias` in existence is already in the form the resolver compares, which
    /// is what makes an exported table (`LanguageResolver::aliases`) readable
    /// and reloadable without a second normalization pass.
    #[must_use]
    pub fn new(question: &str, utterance: &str, option: &str) -> Self {
        Self {
            question: question.to_owned(),
            utterance: normalize(utterance),
            option: option.to_owned(),
        }
    }

    /// Returns the question this binding is scoped to.
    #[must_use]
    pub fn question(&self) -> &str {
        &self.question
    }

    /// Returns the confirmed phrase, normalized.
    #[must_use]
    pub fn utterance(&self) -> &str {
        &self.utterance
    }

    /// Returns the bound option text.
    #[must_use]
    pub fn option(&self) -> &str {
        &self.option
    }
}

/// Resolves a whole-number answer by value, with the sign intact.
///
/// The typed counterpart of the tiered lexical search, and deliberately not a
/// tier of it. Every tier below folds text, and the fold is what makes a numeric
/// question dangerous: [`normalize`] turns punctuation into a separator, so
/// `normalize("-5") == normalize("5")`, and a lexical tier then reports the
/// negative answer as an `Exact` match of the positive option with score `1.0`.
/// Comparing decoded [`i64`]s cannot do that: `-5 != 5`, and no amount of
/// spelling similarity between two numerals makes them the same number.
///
/// The four outcomes are the same four the lexical path reports, computed over
/// values:
///
/// - the utterance decodes and one option holds that value — `Resolved`,
/// - several options do (`"5"` and `"05"`) — `Ambiguous`, both tied,
/// - it decodes but no option holds it — `Absent`, and *no fallback*: an
///   out-of-set number is not lexically close to the numbers that are offered,
///   it is simply not offered,
/// - it does not decode at all — `Absent`, with no phonetic or fuzzy attempt,
///   because the answer to a numeric question is the number.
///
/// The alias table is not consulted either, and that is not an oversight: an
/// alias key is a *normalized* utterance, so the lookup for `-5` is the same
/// lookup as for `5` — routing a numeric answer through it would reintroduce
/// exactly the sign loss this path exists to remove.
fn decide_integer(utterance: &str, question: &Question<'_>, margin: f64) -> Resolution {
    let Some(spoken) = decode_integer(utterance) else {
        return Resolution::Absent { best_score: 0.0 };
    };
    let scored: Vec<(usize, MatchTier, f64)> = question
        .options()
        .iter()
        .enumerate()
        .filter_map(|(index, option)| {
            (decode_integer(option) == Some(spoken)).then_some((index, MatchTier::Exact, 1.0))
        })
        .collect();
    decide(&scored, MATCH_THRESHOLD, margin)
}

/// Decides a [`Resolution`] from measured candidates, best-first.
///
/// Shared with the semantic tier rather than reimplemented there. The rule it
/// encodes — the best candidate must clear `threshold`, and must then lead the
/// runner-up by `margin`, with everything inside that margin still in play — is
/// the *policy* of a three-way verdict, and two copies of it would be two
/// policies that drift. Only the two constants differ between callers, so both
/// are parameters.
///
/// `scored` must carry **every candidate that was measured**, including those
/// below `threshold`. The two questions are separate and the order they are
/// asked in is the whole contract. The threshold asks *may this option win?*
/// and it answers that question about one option at a time. The margin asks
/// *did the winner separate itself from the next best thing actually observed?*
/// and that question cannot be answered against a field the threshold has
/// already emptied. Filtering first and measuring second is what lets a
/// competitor on the far side of the threshold manufacture confidence: an
/// option measured at `0.71` against a threshold of `0.72` disappears, and a
/// winner at `0.73` then reports a lead of `0.73` over nothing instead of the
/// `0.02` it actually holds.
///
/// A shared `decide` over pre-filtered input cannot be fixed by the caller
/// choosing a better threshold. It is the ordering that is wrong.
///
/// The field `scored` is measured over is the **winning tier's**, because
/// `score_all` has already reduced the candidate set to the one tier that
/// produced anything. The lead is therefore measured **within** a tier and
/// never across tiers: the numbers are not comparable, and a fuzzy competitor
/// is not a rival for an exact answer. A unique exact winner consequently
/// reports `lead == score`, exactly as a lone option does — the field really is
/// empty of rivals, and the tie that matters is between the two exact
/// candidates when there are two. Two exact candidates therefore tie at
/// `lead == 0.0` however far the nearest fuzzy competitor is.
pub(crate) fn decide(
    scored: &[(usize, MatchTier, f64)],
    threshold: f64,
    margin: f64,
) -> Resolution {
    let Some(&(index, tier, score)) = scored.first() else {
        return Resolution::Absent { best_score: 0.0 };
    };
    if score < threshold {
        // Nothing was accepted. The maximum is still reported rather than
        // `0.0`: the closest option *was* measured, and `0.0` is a value no
        // comparison produced — the same claim [`Resolution::Degraded`] refuses
        // to make about a resolver that never ran.
        return Resolution::Absent { best_score: score };
    }
    // The runner-up is the highest *other* score observed, including one below
    // the threshold. An option that may not win is still an option that was
    // measured, and its proximity is evidence about the winner.
    let lead = match scored.get(1) {
        Some(&(_, _, next)) => score - next,
        None => score,
    };
    if lead >= margin {
        return Resolution::Resolved {
            index,
            tier,
            score,
            lead,
        };
    }
    // Everything within the margin of the best is still in play, and the caller
    // narrows the re-ask to exactly that set. Ascending index order, as
    // [`Resolution::Ambiguous::tied`] documents: the caller's list order must
    // not change the verdict, and an index list is compared by its contents.
    let mut tied: Vec<usize> = scored
        .iter()
        .take_while(|candidate| score - candidate.2 < margin)
        .map(|candidate| candidate.0)
        .collect();
    tied.sort_unstable();
    Resolution::Ambiguous { tied, tier, score }
}

/// The shipped lexicon resolver: tiered matching plus learned aliases.
///
/// `Debug` prints the learned table, the resolver's whole mutable state, in
/// `BTreeMap` key order — so the rendering is stable and diffable across runs.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct LanguageResolver {
    /// Learned aliases: question id, then normalized utterance.
    ///
    /// Two levels rather than one flat key because the question is a *scope*,
    /// not part of a compound name: resolving asks "what does this phrase mean
    /// here", which is one lookup in the inner table, and exporting a question's
    /// whole vocabulary is one iteration of it.
    aliases: BTreeMap<String, BTreeMap<String, Alias>>,
    /// The bounded lexical metric used by the fuzzy tier.
    distance: EditDistance,
}

impl LanguageResolver {
    /// Creates a resolver with no learned aliases.
    #[must_use]
    pub fn new() -> Self {
        Self {
            aliases: BTreeMap::new(),
            distance: EditDistance::new(MAX_UTTERANCE_CHARS),
        }
    }

    /// Creates a resolver pre-loaded with learned aliases.
    ///
    /// The aliases are supplied rather than fitted so an estate can ship a
    /// house vocabulary as data, and a reviewer can read the whole of it. Each
    /// row carries its own question scope, so a migrated table keeps its
    /// bindings to the questions they were confirmed in rather than being
    /// flattened onto whatever question is asked first.
    #[must_use]
    pub fn with_aliases(aliases: Vec<Alias>) -> Self {
        let mut resolver = Self::new();
        for alias in aliases {
            let table = resolver.aliases.entry(alias.question.clone()).or_default();
            table.insert(alias.utterance.clone(), alias);
        }
        resolver
    }

    /// Records that `utterance` means `option` *in `question`*.
    ///
    /// This is the vocabulary axis of linguistic change: the caller calls it
    /// when a resolution was confirmed, so next time the phrase is exact rather
    /// than a fuzzy guess. A row is readable and revocable, which a fitted
    /// weight is not.
    ///
    /// Returns the binding this one replaced, if the phrase was already learned
    /// in this question — so re-teaching a phrase is a visible change to a
    /// confirmation, not a silent one. That returned row is the provenance: the
    /// caller can see it was taught `Cancel` before it was taught `Repeat last
    /// order`, which is the difference between a correction and a first lesson.
    /// Re-teaching a phrase in a *different* question returns `None` and does
    /// not disturb the first question's row.
    pub fn learn(&mut self, question: &str, utterance: &str, option: &str) -> Option<Alias> {
        let alias = Alias::new(question, utterance, option);
        self.aliases
            .entry(alias.question.clone())
            .or_default()
            .insert(alias.utterance.clone(), alias)
    }

    /// Forgets one question's binding for a phrase, returning the row removed.
    ///
    /// Returns the removed [`Alias`] rather than a `bool` so a caller that
    /// revokes a confirmation — deleting a row from the alias table — can
    /// record what was revoked; `None` means there was nothing to revoke.
    pub fn forget(&mut self, question: &str, utterance: &str) -> Option<Alias> {
        let spoken = normalize(utterance);
        let removed = self
            .aliases
            .get_mut(question)
            .and_then(|table| table.remove(&spoken));
        if self.aliases.get(question).is_some_and(BTreeMap::is_empty) {
            self.aliases.remove(question);
        }
        removed
    }

    /// Returns the number of learned aliases across every question.
    #[must_use]
    pub fn learned(&self) -> usize {
        self.aliases.values().map(BTreeMap::len).sum()
    }

    /// Returns every learned alias, question by question.
    ///
    /// The table as data, so the vocabulary a resolver has learned can be
    /// reviewed or migrated into another resolver — `with_aliases` accepts
    /// exactly what this yields, which is what makes a shipped house vocabulary
    /// and a learned one the same kind of thing.
    #[must_use]
    pub fn aliases(&self) -> Vec<Alias> {
        self.aliases
            .values()
            .flat_map(BTreeMap::values)
            .cloned()
            .collect()
    }

    /// Returns the binding for `utterance` in this question, when the option it
    /// names is **no longer offered**.
    ///
    /// The superseded case, isolated from scoring so it can be reported instead
    /// of scored. A binding whose option is gone has nothing to say about the
    /// current list: it must not be spent on the option that took its index, and
    /// it must not be quietly dropped either, because the person is still using
    /// a phrase whose meaning was confirmed and has now been withdrawn without
    /// anyone telling them.
    fn superseded<'a>(&'a self, question: &Question<'_>, utterance: &str) -> Option<&'a Alias> {
        let spoken = normalize(utterance);
        let alias = self.aliases.get(question.id())?.get(&spoken)?;
        if question
            .options()
            .iter()
            .any(|option| option == alias.option())
        {
            return None;
        }
        Some(alias)
    }

    /// Resolves `utterance` against `question` as a four-way verdict.
    ///
    /// The question's [`AnswerDomain`] chooses the reading, and it is a branch
    /// rather than a tier because the two readings are not comparable: lexical
    /// similarity measures how alike two strings look, and a number is not
    /// answered by how much it looks like another number.
    #[must_use]
    pub fn decide_for(&self, utterance: &str, question: &Question<'_>) -> Resolution {
        if question.domain() == AnswerDomain::Integer {
            return decide_integer(utterance, question, MATCH_MARGIN);
        }
        let verdict = decide(
            &score_all(utterance, question, &self.aliases, &self.distance),
            MATCH_THRESHOLD,
            MATCH_MARGIN,
        );
        // Staleness is reported only where it would otherwise decide the
        // outcome. A superseded alias leaves the scorer with nothing to say, so
        // `Absent` is the verdict it produces; that is the case where naming the
        // cause is strictly more useful than reporting an unclear answer. If the
        // current list *did* produce a reading — the phrase is literally on
        // screen — that reading stands, because withdrawing a correct answer to
        // report a fact about the alias table would be the tail wagging the dog.
        if !matches!(verdict, Resolution::Absent { .. }) {
            return verdict;
        }
        match self.superseded(question, utterance) {
            Some(alias) => Resolution::StaleAlias {
                question: alias.question().to_owned(),
                option: alias.option().to_owned(),
            },
            None => verdict,
        }
    }

    /// The version of the lexicon policy in force.
    ///
    /// Everything that can change which candidate wins is in the digest: the
    /// two tier weights, the acceptance threshold, the required lead, and the
    /// input length past which the distance metric stops scoring at all. A
    /// parameter left out here would be a change no receipt could see, which is
    /// the same omission at a smaller scale as the one this field exists to
    /// remove.
    #[must_use]
    pub fn policy_version(&self) -> PolicyVersion {
        PolicyVersion::new(
            POLICY_LABEL,
            &[
                MATCH_THRESHOLD,
                MATCH_MARGIN,
                TOKEN_WEIGHT,
                DISTANCE_WEIGHT,
                f64::from(INPUT_BOUND_CHARS),
            ],
        )
    }
}

impl Default for LanguageResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::session::Resolver for LanguageResolver {
    fn resolve(&self, utterance: &str, question: &Question<'_>) -> Verdict {
        Verdict::new(
            self.decide_for(utterance, question),
            Provenance::without_model(self.policy_version()),
        )
    }
}

#[cfg(test)]
mod tests;
