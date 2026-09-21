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

use std::collections::BTreeMap;

use lgwks_std::similarity::{EditDistance, Jaccard, Similarity};

use crate::session::{
    AnswerDomain, MatchTier, PolicyVersion, Provenance, Question, Resolution, Verdict,
    decode_integer,
};

/// The longest normalized input the shipped resolver will compare.
pub const MAX_UTTERANCE_CHARS: usize = 512;

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
    // Stable sort by descending score. `sort_by` is stable, and the input is in
    // ascending index order, so equal scores keep lowest-index-first for free.
    scored.sort_by(|left, right| {
        right
            .2
            .partial_cmp(&left.2)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
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

    /// Returns the version of the lexicon policy in force.
    ///
    /// Everything that can change which candidate wins is in the digest: the
    /// two tier weights, the acceptance threshold, the required lead, and the
    /// input length past which the distance metric stops scoring at all. A
    /// parameter left out here would be a change no receipt could see, which is
    /// the same omission at a smaller scale as the one this field exists to
    /// remove.
    #[must_use]
    pub fn policy_version(&self) -> PolicyVersion {
        // The input bound is a `usize`, converted with `try_from` rather than a
        // truncating `as` because this workspace forbids the cast; the length
        // is a compile-time 512, so the fallback is unreachable and exists only
        // so the conversion has no panic path.
        let input_bound = f64::from(u32::try_from(MAX_UTTERANCE_CHARS).unwrap_or(u32::MAX));
        PolicyVersion::new(
            POLICY_LABEL,
            &[
                MATCH_THRESHOLD,
                MATCH_MARGIN,
                TOKEN_WEIGHT,
                DISTANCE_WEIGHT,
                input_bound,
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
mod tests {
    use super::*;
    use crate::session::Resolver;

    /// The sentence the cross-tier tests resolve, and the two competitors that
    /// sit below an exact match of it.
    ///
    /// `IDENTICAL` is the utterance itself. `PHONETIC_TWIN` keeps a *different*
    /// spelling of one word, so it lands on the phonetic tier, and `FUZZY_TWIN`
    /// transposes the first two words, which leaves the token set identical and
    /// the edit distance at four characters — a fuzzy score above the phonetic
    /// tier's `0.9`, which is exactly the ordering these tests exist to pin.
    const IDENTICAL: &str = "a monthly account statement should be sent to my email address";
    const PHONETIC_TWIN: &str = "a monthlee account statement should be sent to my email address";
    const FUZZY_TWIN: &str = "monthly a account statement should be sent to my email address";

    /// Builds the option list most tests resolve against.
    fn options() -> Vec<String> {
        vec![
            String::from("Yes, continue"),
            String::from("No, go back"),
            String::from("Speak to a person"),
        ]
    }

    /// Names the question most tests resolve against.
    fn ask(options: &[String]) -> Question<'_> {
        Question::new("ask", options)
    }

    /// Returns the option and tier a verdict selected, or `None` for any other
    /// verdict — so a test can assert identity and tier in one comparison
    /// without a `panic!`, which this workspace forbids.
    fn selected(verdict: &Verdict) -> Option<(usize, MatchTier)> {
        match *verdict.resolution() {
            Resolution::Resolved { index, tier, .. } => Some((index, tier)),
            _ => None,
        }
    }

    /// Returns the tied options and the tier they tied in, or `None`.
    fn tied(verdict: &Verdict) -> Option<(Vec<usize>, MatchTier)> {
        match *verdict.resolution() {
            Resolution::Ambiguous { ref tied, tier, .. } => Some((tied.clone(), tier)),
            _ => None,
        }
    }

    /// Asserts two scores agree to within the tolerance float arithmetic needs.
    fn assert_close(left: f64, right: f64) {
        assert!(
            (left - right).abs() < 1e-9,
            "expected {right}, observed {left}"
        );
    }

    #[test]
    fn normalization_folds_case_punctuation_and_accents() {
        assert_eq!(normalize("  Yes,  CONTINUE! "), "yes continue");
        assert_eq!(normalize("Café"), "cafe");
        assert_eq!(
            normalize("Œuvre"),
            "ouvre",
            "a ligature folds to its base letter, not its expansion"
        );
        assert_eq!(normalize("Straße"), "strase");
        assert_eq!(normalize("!!!"), "");
    }

    #[test]
    fn normalization_treats_unsupported_scripts_as_passthrough() {
        // Not a failure: the character survives so the fuzzy tier can still see
        // it, rather than being silently dropped to nothing.
        assert_eq!(normalize("日本"), "日本");
    }

    #[test]
    fn phonetic_keys_collapse_spelling_variation() {
        assert_eq!(phonetic_key("Smith"), phonetic_key("Smyth"));
        assert_eq!(phonetic_key("there"), phonetic_key("their"));
        assert_eq!(phonetic_key("Straße"), phonetic_key("Strasse"));
        assert_eq!(phonetic_key("Kathryn"), phonetic_key("Kathrin"));
    }

    #[test]
    fn phonetic_keys_preserve_the_initial_and_so_do_not_collapse_catherine() {
        // Recorded as a test rather than left implicit: this is the deliberate
        // false-negative side of the Soundex trade, and a future change that
        // "fixes" it would also collapse `cat`/`sat`.
        assert_eq!(phonetic_key("Catherine"), "C365");
        assert_eq!(phonetic_key("Kathryn"), "K365");
        // Same tail, different initial: not collapsed, which is the same rule.
        assert_ne!(phonetic_key("cat"), phonetic_key("kat"));
        assert_ne!(phonetic_key("cat"), phonetic_key("sat"));
    }

    #[test]
    fn phonetic_keys_distinguish_unrelated_words() {
        assert_ne!(phonetic_key("continue"), phonetic_key("person"));
    }

    #[test]
    fn an_input_with_no_letters_has_an_empty_phonetic_key() {
        assert_eq!(phonetic_key("123"), "");
        assert_eq!(phonetic_key("!!!"), "");
    }

    /// Whether a resolved verdict's lead clears the margin — the property that
    /// makes `Resolved` the right variant rather than `Ambiguous`.
    ///
    /// Note what this does *not* assert. A lead equal to the score is not by
    /// itself evidence of the discarded-runner-up defect: when the best score
    /// is `0.9` and the runner-up legitimately measured `0.0`, the gap really is
    /// `0.9`. The two states are told apart by the score the runner-up was
    /// given, not by the width of the gap, which is why the regression tests
    /// below build the candidate field explicitly instead of inferring it from a
    /// real phrase pair.
    fn clears_the_margin(lead: f64) -> bool {
        lead >= MATCH_MARGIN
    }

    #[test]
    fn an_exact_normalized_match_resolves_at_the_exact_tier() {
        let resolution = LanguageResolver::new()
            .resolve("  yes, CONTINUE ", &ask(&options()))
            .into_resolution();
        // The other two options only ever reach the fuzzy tier, and `score_all`
        // reduces the field to the winning tier before `decide` measures
        // anything, so the rival field this winner is measured against is empty
        // and the lead is the whole of its score — the same honest equality a
        // lone option reports. It is not the discarded-runner-up defect: that was
        // a *same-tier* competitor deleted by the threshold before the lead was
        // taken, and `the_lead_is_measured_against_the_highest_other_measured_score`
        // below pins the arithmetic that fixes it. Cross-tier, the gap would be
        // a subtraction between two numbers that do not mean the same thing.
        assert!(
            matches!(
                resolution,
                Resolution::Resolved {
                    index: 0,
                    tier: MatchTier::Exact,
                    score,
                    lead,
                } if (score - 1.0).abs() < 1e-9
                    && (lead - score).abs() < 1e-9
                    && clears_the_margin(lead)
            ),
            "expected an exact resolution of option 0 over a field with no exact rival, got {resolution:?}"
        );
    }

    #[test]
    fn a_spelling_variation_resolves_at_the_phonetic_tier() {
        let choices = vec![String::from("Smyth"), String::from("Marcus")];
        let resolution = LanguageResolver::new()
            .resolve("Smith", &ask(&choices))
            .into_resolution();
        assert_eq!(
            resolution,
            Resolution::Resolved {
                index: 0,
                tier: MatchTier::Phonetic,
                score: 0.9,
                lead: 0.9,
            }
        );
    }

    #[test]
    fn an_unrecognized_answer_is_absent() {
        let resolution = LanguageResolver::new()
            .resolve("maybe later", &ask(&options()))
            .into_resolution();
        assert!(
            matches!(resolution, Resolution::Absent { .. }),
            "expected Absent, got {resolution:?}"
        );
    }

    #[test]
    fn two_equally_close_options_are_ambiguous_and_both_are_tied() {
        // Both options share a sound-alike key with the utterance, so both are
        // phonetic candidates at the same score and neither leads.
        let choices = vec![
            String::from("Accept the offer"),
            String::from("Accept the order"),
        ];
        let verdict = LanguageResolver::new().resolve("accept the", &ask(&choices));
        assert_eq!(
            tied(&verdict),
            Some((vec![0, 1], MatchTier::Phonetic)),
            "both options must stay in play, and the tie names its tier: {verdict:?}"
        );
    }

    #[test]
    fn a_fuzzy_tie_is_reported_at_the_fuzzy_tier() {
        // The margin still has its intended meaning *inside* a tier: two
        // candidates that fold to the same text score identically, and the
        // resolver reports the tie rather than letting index order decide.
        let choices = vec![
            String::from(FUZZY_TWIN),
            String::from("monthly a account statement should be sent to my email address!"),
        ];
        let resolution = LanguageResolver::new().resolve(IDENTICAL, &ask(&choices));
        assert_eq!(
            tied(&resolution),
            Some((vec![0, 1], MatchTier::Fuzzy)),
            "two equally fuzzy candidates stay ambiguous: {resolution:?}"
        );
    }

    #[test]
    fn a_learned_alias_resolves_exactly_and_is_revocable() {
        let mut resolver = LanguageResolver::new();
        assert_eq!(resolver.learned(), 0);
        assert_eq!(
            resolver.learn("ask", "the usual", "Speak to a person"),
            None,
            "a fresh alias"
        );
        assert_eq!(resolver.learned(), 1);

        assert_eq!(
            resolver
                .resolve("The usual!", &ask(&options()))
                .into_resolution(),
            Resolution::Resolved {
                index: 2,
                tier: MatchTier::Exact,
                score: 1.0,
                lead: 1.0,
            },
            "an alias is a confirmed exact match, not a fuzzy guess"
        );

        let replaced = resolver.learn("ask", "the usual", "No, go back");
        assert_eq!(
            replaced.as_ref().map(Alias::option),
            Some("Speak to a person"),
            "re-teaching returns the binding it replaced, so a correction is \
             distinguishable from a first lesson"
        );
        assert_eq!(
            resolver
                .forget("ask", "the usual")
                .as_ref()
                .map(Alias::option),
            Some("No, go back"),
            "forgetting a learned alias returns the row it revoked"
        );
        assert_eq!(
            resolver.forget("ask", "the usual"),
            None,
            "forgetting a learned alias twice revokes nothing"
        );
        assert_eq!(resolver.learned(), 0);
    }

    #[test]
    fn a_shipped_alias_table_is_normalized_on_load() {
        let resolver = LanguageResolver::with_aliases(vec![Alias::new(
            "ask",
            "  The Usual ",
            "Speak to a person",
        )]);
        assert_eq!(
            resolver
                .resolve("the usual", &ask(&options()))
                .into_resolution(),
            Resolution::Resolved {
                index: 2,
                tier: MatchTier::Exact,
                score: 1.0,
                lead: 1.0,
            }
        );
    }

    /// The filed counterexample for #25, first half: the confirmation was made
    /// about an option, not about row 0, so it follows the option when the flow
    /// reorders its choices.
    #[test]
    fn an_alias_follows_its_option_to_a_new_position() {
        let resolver = LanguageResolver::with_aliases(vec![Alias::new(
            "ask",
            "the usual",
            "Repeat last order",
        )]);
        for (choices, expected) in [
            (vec!["Repeat last order", "Cancel"], 0_usize),
            (vec!["Cancel", "Repeat last order"], 1),
        ] {
            let options = choices
                .iter()
                .map(|name| (*name).to_owned())
                .collect::<Vec<_>>();
            let verdict = resolver.resolve("the usual", &ask(&options));
            assert_eq!(
                selected(&verdict),
                Some((expected, MatchTier::Exact)),
                "the confirmation must travel with its option, not its index: \
                 {choices:?} gave {verdict:?}"
            );
        }
    }

    /// The filed counterexample for #25, second half: a binding made in one
    /// question's vocabulary is not spent in another's — even when the other
    /// question offers the identical option list. Without the scope this is
    /// `Resolved` at index 0 with score 1.0, which is how `"the usual"` became
    /// `"Delete account"`.
    #[test]
    fn an_alias_is_not_visible_from_another_question() {
        let resolver = LanguageResolver::with_aliases(vec![Alias::new(
            "ask",
            "the usual",
            "Repeat last order",
        )]);
        let choices = vec![
            String::from("Repeat last order"),
            String::from("Cancel"),
            String::from("Delete account"),
        ];
        let elsewhere = resolver.resolve("the usual", &Question::new("some_other_ask", &choices));
        assert_eq!(
            selected(&elsewhere),
            None,
            "another question must not consume this question's confirmation: {elsewhere:?}"
        );
        assert!(
            matches!(
                elsewhere.resolution(),
                Resolution::Absent { best_score }
                    if *best_score > 0.0 && *best_score < MATCH_THRESHOLD
            ),
            "the phrase is simply unrecognized there — measured, and nowhere near \
             the threshold — not quietly bound to row 0: {elsewhere:?}"
        );
    }

    /// An option that is removed does not hand its confirmation to the option
    /// that replaces it, and the withdrawal is reported rather than swallowed.
    #[test]
    fn a_superseded_alias_is_reported_rather_than_spent_on_the_new_option() {
        let resolver = LanguageResolver::with_aliases(vec![Alias::new(
            "ask",
            "the usual",
            "Repeat last order",
        )]);
        let choices = vec![String::from("Delete account"), String::from("Keep account")];
        let verdict = resolver.resolve("the usual", &ask(&choices));
        assert_eq!(
            verdict.resolution(),
            &Resolution::StaleAlias {
                question: String::from("ask"),
                option: String::from("Repeat last order"),
            },
            "the binding names the option that went away, not the one at its old index"
        );
        assert!(
            !matches!(verdict.resolution(), Resolution::Resolved { .. }),
            "a withdrawn confirmation must never select an option"
        );
    }

    /// Staleness is reported where it would decide the outcome, not ahead of an
    /// answer the current list genuinely supports.
    #[test]
    fn a_visible_option_outranks_a_superseded_alias() {
        let resolver =
            LanguageResolver::with_aliases(vec![Alias::new("ask", "yes", "No, go back")]);
        let choices = vec![String::from("Yes")];
        let verdict = resolver.resolve("yes", &ask(&choices));
        assert_eq!(
            selected(&verdict),
            Some((0, MatchTier::Exact)),
            "the phrase is on screen and the reading does not come from the \
             alias at all; refusing an answer the person can see to report a \
             fact about the alias table would be the tail wagging the dog: \
             {verdict:?}"
        );
    }

    /// The same phrase, the same option list, two questions: the meaning is a
    /// property of the question, which is what the scope makes expressible.
    #[test]
    fn one_phrase_can_mean_two_things_in_two_questions() {
        let mut resolver = LanguageResolver::new();
        resolver.learn("ask_one", "the usual", "Repeat last order");
        assert_eq!(
            resolver.learn("ask_two", "the usual", "Cancel"),
            None,
            "a second question's first lesson has no predecessor in *that* question"
        );
        assert_eq!(resolver.learned(), 2, "both questions kept their own row");

        let choices = vec![String::from("Repeat last order"), String::from("Cancel")];
        assert_eq!(
            selected(&resolver.resolve("the usual", &Question::new("ask_one", &choices))),
            Some((0, MatchTier::Exact)),
            "in the first question the phrase means the first option"
        );
        assert_eq!(
            selected(&resolver.resolve("the usual", &Question::new("ask_two", &choices))),
            Some((1, MatchTier::Exact)),
            "in the second question it means the second"
        );
    }

    /// Two spellings that fold to one form are one row, and the row it replaced
    /// is returned rather than dropped.
    #[test]
    fn a_normalized_collision_rebinds_one_row_and_returns_the_replaced_one() {
        let mut resolver = LanguageResolver::new();
        resolver.learn("ask", "The Usual", "Speak to a person");
        let replaced = resolver.learn("ask", "the usual!", "No, go back");
        assert_eq!(
            replaced.as_ref().map(Alias::option),
            Some("Speak to a person"),
            "the collision is a rebinding of one phrase, and it is visible"
        );
        assert_eq!(resolver.learned(), 1, "one phrase is one row");
        assert_eq!(
            selected(&resolver.resolve("the usual", &ask(&options()))),
            Some((1, MatchTier::Exact)),
            "the later binding is the live one"
        );
    }

    /// The learned vocabulary is data, so it can be reviewed and moved. A table
    /// that survives the round trip is the difference between a house vocabulary
    /// an estate can ship and one trapped inside a running process.
    #[test]
    fn an_exported_table_reloads_unchanged() {
        let mut resolver = LanguageResolver::new();
        resolver.learn("ask", "the usual", "Repeat last order");
        resolver.learn("ask", "nah", "Cancel");

        let table = resolver.aliases();
        assert_eq!(table.len(), 2, "both rows export");
        assert!(
            table.iter().any(|alias| alias.question() == "ask"
                && alias.utterance() == "the usual"
                && alias.option() == "Repeat last order"),
            "a row is readable without knowing how it was stored: {table:?}"
        );

        let reloaded = LanguageResolver::with_aliases(table);
        assert_eq!(reloaded.learned(), resolver.learned());
        let choices = vec![String::from("Repeat last order"), String::from("Cancel")];
        for utterance in ["the usual", "nah"] {
            assert_eq!(
                reloaded.resolve(utterance, &ask(&choices)),
                resolver.resolve(utterance, &ask(&choices)),
                "a migrated table resolves exactly as the one it came from"
            );
        }
    }

    #[test]
    fn the_fuzzy_weights_account_for_the_whole_score() {
        assert_close(TOKEN_WEIGHT + DISTANCE_WEIGHT, 1.0);
    }

    #[test]
    fn a_long_utterance_degrades_rather_than_panicking() {
        let long = "yes ".repeat(MAX_UTTERANCE_CHARS);
        // `EditDistance` refuses an over-limit input and scores it 0.0 rather
        // than allocating a quadratic table, so a hostile input degrades to the
        // token tier instead of hanging. The bound is the contract.
        let resolution = LanguageResolver::new()
            .resolve(&long, &ask(&options()))
            .into_resolution();
        assert!(
            matches!(
                resolution,
                Resolution::Resolved { .. } | Resolution::Absent { .. }
            ),
            "an over-limit input must produce a verdict, got {resolution:?}"
        );
    }

    #[test]
    fn an_empty_option_list_is_absent_not_a_panic() {
        assert_eq!(
            LanguageResolver::new()
                .resolve("yes", &ask(&[]))
                .into_resolution(),
            Resolution::Absent { best_score: 0.0 }
        );
    }

    /// The `FUZZY_TWIN` reading of `IDENTICAL` must really be a *fuzzy* reading
    /// scoring above the phonetic tier, or the precedence tests below would be
    /// comparing nothing. This test states that premise so a change to the
    /// similarity arithmetic cannot quietly hollow them out.
    #[test]
    fn the_fuzzy_competitor_really_outscores_the_phonetic_tier() {
        let alone = vec![String::from(FUZZY_TWIN)];
        let verdict = LanguageResolver::new().resolve(IDENTICAL, &ask(&alone));
        assert!(
            matches!(
                verdict.resolution(),
                Resolution::Resolved {
                    tier: MatchTier::Fuzzy,
                    score,
                    ..
                } if *score > 0.9
            ),
            "the fuzzy competitor must score above the phonetic tier's 0.9: {verdict:?}"
        );
    }

    #[test]
    fn an_exact_answer_is_not_vetoed_by_a_fuzzy_competitor() {
        // The filed counterexample: the person typed the complete text of
        // option 0, and a transposed competitor one tier down scored high
        // enough to sit inside the margin.
        let choices = vec![String::from(IDENTICAL), String::from(FUZZY_TWIN)];
        let verdict = LanguageResolver::new().resolve(IDENTICAL, &ask(&choices));
        assert_eq!(
            selected(&verdict),
            Some((0, MatchTier::Exact)),
            "a unique exact answer must not be vetoed by a lower tier: {verdict:?}"
        );
    }

    #[test]
    fn an_exact_answer_is_not_vetoed_by_a_phonetic_competitor() {
        let choices = vec![String::from(IDENTICAL), String::from(PHONETIC_TWIN)];
        let verdict = LanguageResolver::new().resolve(IDENTICAL, &ask(&choices));
        assert_eq!(
            selected(&verdict),
            Some((0, MatchTier::Exact)),
            "the phonetic tier ranks below the exact tier: {verdict:?}"
        );
    }

    #[test]
    fn a_phonetic_candidate_outranks_a_higher_scoring_fuzzy_candidate() {
        let choices = vec![String::from(PHONETIC_TWIN), String::from(FUZZY_TWIN)];
        let verdict = LanguageResolver::new().resolve(IDENTICAL, &ask(&choices));
        assert_eq!(
            selected(&verdict),
            Some((0, MatchTier::Phonetic)),
            "precedence is the tier order, not the numeric score: {verdict:?}"
        );
    }

    #[test]
    fn two_normalized_equal_exact_options_tie_rather_than_taking_the_first() {
        // Both options fold to `yes continue`, so both are exact. Picking the
        // first would be an arbitrary choice between two options the person's
        // answer genuinely does not distinguish.
        let choices = vec![String::from("Yes, continue"), String::from("yes continue!")];
        let verdict = LanguageResolver::new().resolve("yes continue", &ask(&choices));
        assert_eq!(
            tied(&verdict),
            Some((vec![0, 1], MatchTier::Exact)),
            "an exact collision stays ambiguous at the exact tier: {verdict:?}"
        );
    }

    #[test]
    fn a_learned_alias_conflicting_with_a_normalized_exact_option_ties() {
        // The confirmation says `yes` means `No`; the folding says it means
        // `Yes`. Both are exact-tier readings, so the resolver reports the
        // conflict rather than letting the alias table silently outrank the
        // option text.
        let mut resolver = LanguageResolver::new();
        resolver.learn("ask", "yes", "No");
        let choices = vec![String::from("Yes"), String::from("No")];
        let verdict = resolver.resolve("yes", &ask(&choices));
        assert_eq!(
            tied(&verdict),
            Some((vec![0, 1], MatchTier::Exact)),
            "a conflicting confirmation is a tie, not an override: {verdict:?}"
        );
    }

    #[test]
    fn tier_precedence_survives_option_permutation() {
        let resolver = LanguageResolver::new();
        for (choices, expected) in [
            (
                vec![String::from(IDENTICAL), String::from(FUZZY_TWIN)],
                (0_usize, MatchTier::Exact),
            ),
            (
                vec![String::from(FUZZY_TWIN), String::from(IDENTICAL)],
                (1, MatchTier::Exact),
            ),
            (
                vec![String::from(PHONETIC_TWIN), String::from(FUZZY_TWIN)],
                (0, MatchTier::Phonetic),
            ),
            (
                vec![String::from(FUZZY_TWIN), String::from(PHONETIC_TWIN)],
                (1, MatchTier::Phonetic),
            ),
        ] {
            let verdict = resolver.resolve(IDENTICAL, &ask(&choices));
            assert_eq!(
                selected(&verdict),
                Some(expected),
                "the winning option must follow its text, not its position: {choices:?} gave {verdict:?}"
            );
        }
    }

    /// Builds an option list from literals, for the numeric-domain tests below.
    fn amounts(choices: &[&str]) -> Vec<String> {
        choices.iter().map(|choice| (*choice).to_owned()).collect()
    }

    /// Names an integer question over `options`.
    fn numeric(options: &[String]) -> Question<'_> {
        Question::new("amount", options).with_domain(AnswerDomain::Integer)
    }

    #[test]
    fn integer_decoding_trims_surrounding_space_and_reads_a_leading_sign() {
        for (raw, expected) in [
            ("5", Some(5_i64)),
            ("+5", Some(5)),
            ("-5", Some(-5)),
            (" 5 ", Some(5)),
            ("0", Some(0)),
            ("-0", Some(0)),
            ("007", Some(7)),
            ("", None),
            ("five", None),
            ("5.0", None),
            ("5,000", None),
            ("9223372036854775807", Some(i64::MAX)),
            ("-9223372036854775808", Some(i64::MIN)),
            ("9223372036854775808", None),
            ("-9223372036854775809", None),
        ] {
            assert_eq!(
                decode_integer(raw),
                expected,
                "{raw:?} must decode to {expected:?} and nothing else"
            );
        }
    }

    #[test]
    fn the_label_fold_is_exactly_why_a_number_needs_its_own_domain() {
        // The premise of the defect, stated as an assertion: the normalization
        // every label match runs through erases the sign, so a numeric answer
        // read as a label cannot tell "-5" from "5".
        assert_eq!(
            normalize("-5"),
            normalize("5"),
            "the fold collapses the sign, which is why it must not be applied to a value"
        );
        // And that is what the label domain does with the same two options: it
        // reports a tie rather than claiming either one. (A validated flow
        // refuses to offer them as labels at all — see `session.rs`.)
        let options = amounts(&["-5", "5"]);
        let verdict = LanguageResolver::new().resolve("-5", &ask(&options));
        assert_eq!(
            tied(&verdict),
            Some((vec![0, 1], MatchTier::Exact)),
            "under the label domain the sign is punctuation: {verdict:?}"
        );
    }

    #[test]
    fn a_numeric_question_selects_the_value_and_never_the_spelling() {
        let resolver = LanguageResolver::new();
        let options = amounts(&["-5", "5", "0", "10"]);
        let question = numeric(&options);
        for (utterance, expected) in [
            ("-5", 0_usize),
            ("5", 1),
            ("+5", 1),
            (" 5 ", 1),
            ("0", 2),
            ("10", 3),
        ] {
            let verdict = resolver.resolve(utterance, &question);
            assert_eq!(
                selected(&verdict),
                Some((expected, MatchTier::Exact)),
                "{utterance:?} must select the option holding that value: {verdict:?}"
            );
        }
    }

    #[test]
    fn a_number_no_option_holds_is_absent_rather_than_a_nearby_option() {
        let resolver = LanguageResolver::new();
        let options = amounts(&["5", "10"]);
        let question = numeric(&options);

        // The filed counterexample, reproduced: the same utterance against the
        // same options, read the way the defect read every question. This is the
        // verdict #38 reported — `-5` becoming option 0 at the Exact tier — and
        // it is asserted rather than removed, because it is the behaviour the
        // domain exists to withhold. Delete the domain and this is what the
        // assertion below starts returning.
        assert_eq!(
            selected(&resolver.resolve("-5", &ask(&options))),
            Some((0, MatchTier::Exact)),
            "the untyped reading of this question is the defect, exactly as filed"
        );

        for utterance in ["-5", "five", "5.0", "9223372036854775808", ""] {
            assert_eq!(
                resolver.resolve(utterance, &question).into_resolution(),
                Resolution::Absent { best_score: 0.0 },
                "{utterance:?} is not one of the offered values and no lexical, \
                 phonetic or fuzzy tier may turn it into one that is"
            );
        }
    }

    #[test]
    fn a_numeric_reading_is_not_vetoed_by_a_similar_number() {
        // No fuzzy competitor exists in this domain: "-50" is not a near miss
        // for "-5", it is a different value.
        let options = amounts(&["-5", "-50"]);
        let verdict = LanguageResolver::new().resolve("-5", &numeric(&options));
        assert_eq!(
            selected(&verdict),
            Some((0, MatchTier::Exact)),
            "the value, not its similarity: {verdict:?}"
        );
    }

    #[test]
    fn a_numeric_question_does_not_consult_the_alias_table() {
        // An alias key is a normalized utterance, and `-5` and `5` share one,
        // so reading a numeric answer through the table would put the sign loss
        // back through a second door.
        let resolver = LanguageResolver::with_aliases(vec![Alias::new("amount", "-5", "5")]);
        assert_eq!(
            resolver.learned(),
            1,
            "the premise: the alias really is held under the folded key"
        );
        let options = amounts(&["5", "10"]);
        assert_eq!(
            resolver.resolve("-5", &numeric(&options)).into_resolution(),
            Resolution::Absent { best_score: 0.0 },
            "the taught phrase must not reach a question read as values"
        );
        assert_eq!(
            selected(&resolver.resolve("5", &numeric(&options))),
            Some((0, MatchTier::Exact)),
            "and the value itself still resolves, by value rather than by alias"
        );
    }

    #[test]
    fn two_options_holding_one_value_tie_rather_than_taking_the_first() {
        // A `FlowSpec` refuses to author this (see `session.rs`); asserted here
        // so the resolver is honest rather than silently positional even when
        // a caller constructs the question directly.
        let options = amounts(&["5", "05"]);
        let verdict = LanguageResolver::new().resolve("5", &numeric(&options));
        assert_eq!(
            tied(&verdict),
            Some((vec![0, 1], MatchTier::Exact)),
            "two options hold 5, so no option is preferred: {verdict:?}"
        );
    }

    /// Builds a measured candidate field from `(index, score)` pairs.
    ///
    /// The tier is irrelevant to the decision arithmetic, so every entry is
    /// `Semantic`: the point of these tests is the relationship between the
    /// scores, and a tier label would be noise a reader has to skip past.
    fn measured(scores: &[(usize, f64)]) -> Vec<(usize, MatchTier, f64)> {
        scores
            .iter()
            .copied()
            .map(|(index, score)| (index, MatchTier::Semantic, score))
            .collect()
    }

    #[test]
    fn the_lead_is_measured_against_the_highest_other_measured_score() {
        let field = measured(&[(0, 0.90), (1, 0.10)]);
        assert_eq!(
            decide(&field, 0.5, 0.08),
            Resolution::Resolved {
                index: 0,
                tier: MatchTier::Semantic,
                score: 0.90,
                lead: 0.80,
            },
            "the lead is the gap to the runner-up, not the score"
        );
    }

    #[test]
    fn a_runner_up_just_below_the_threshold_still_counts_against_the_margin() {
        // The winner clears the threshold by 0.01 and the runner-up misses it by
        // 0.01. The measured lead is 0.02 against a required 0.05, so this is a
        // near tie. Filtering the runner-up away first made `decide` report the
        // winner's whole score as its lead and accept it.
        let field = measured(&[(0, 0.73), (1, 0.71)]);
        assert_eq!(
            decide(&field, 0.72, 0.05),
            Resolution::Ambiguous {
                tied: vec![0, 1],
                tier: MatchTier::Semantic,
                score: 0.73,
            },
            "a competitor below the threshold is still a competitor"
        );
    }

    #[test]
    fn the_straddling_pair_is_not_special_to_a_whole_hundredth() {
        // The same shape with the pair separated from the threshold by an
        // arbitrarily small epsilon: `0.72` exactly is accepted, `0.72 - 1e-12`
        // is not, and either way the *other* measurement is what sets the lead.
        let below = 0.72 - 1e-12;
        let field = measured(&[(0, 0.72), (1, below)]);
        assert_eq!(
            decide(&field, 0.72, 0.05),
            Resolution::Ambiguous {
                tied: vec![0, 1],
                tier: MatchTier::Semantic,
                score: 0.72,
            },
            "a one-epsilon perturbation across the threshold must not manufacture confidence"
        );
    }

    #[test]
    fn a_field_entirely_below_the_threshold_is_absent_at_its_best_measured_score() {
        let field = measured(&[(0, 0.40), (1, 0.30)]);
        assert_eq!(
            decide(&field, 0.72, 0.05),
            Resolution::Absent { best_score: 0.40 },
            "the closest option was measured, so `0.0` would be a value nothing produced"
        );
    }

    #[test]
    fn a_lone_option_reports_its_whole_score_as_the_lead() {
        // There is no runner-up, so the lead is over an empty field and the
        // score is the whole of it. This is the one case where the two are
        // equal and the equality is honest.
        let field = measured(&[(0, 0.80)]);
        assert_eq!(
            decide(&field, 0.72, 0.05),
            Resolution::Resolved {
                index: 0,
                tier: MatchTier::Semantic,
                score: 0.80,
                lead: 0.80,
            }
        );
        assert_eq!(
            decide(&measured(&[(0, 0.50)]), 0.72, 0.05),
            Resolution::Absent { best_score: 0.50 },
            "a lone option that does not clear the threshold is absent, not resolved"
        );
        assert_eq!(
            decide(&[], 0.72, 0.05),
            Resolution::Absent { best_score: 0.0 },
            "no measured candidate at all is the empty field"
        );
    }

    #[test]
    fn an_exact_tie_is_ambiguous_under_a_positive_margin() {
        let field = measured(&[(0, 0.80), (1, 0.80)]);
        assert_eq!(
            decide(&field, 0.72, 0.05),
            Resolution::Ambiguous {
                tied: vec![0, 1],
                tier: MatchTier::Semantic,
                score: 0.80,
            },
            "a tie holds no lead at all"
        );
    }

    #[test]
    fn a_zero_margin_resolves_a_tie_at_the_lowest_index_by_declared_policy() {
        // Margin `0.0` is a caller stating that no separation is required. The
        // arithmetic is left literal rather than special-cased, so `0.0 >= 0.0`
        // accepts the tie and the lowest index wins it — the comparator rule the
        // module documents. The threshold is still enforced first: an exact tie
        // below it is absent, not a coin flip.
        let field = measured(&[(0, 0.80), (1, 0.80)]);
        assert_eq!(
            decide(&field, 0.72, 0.0),
            Resolution::Resolved {
                index: 0,
                tier: MatchTier::Semantic,
                score: 0.80,
                lead: 0.0,
            }
        );
        assert_eq!(
            decide(&measured(&[(0, 0.40), (1, 0.40)]), 0.72, 0.0),
            Resolution::Absent { best_score: 0.40 }
        );
    }

    #[test]
    fn the_tied_set_is_lowest_index_first_whatever_order_it_arrives_in() {
        // `Resolution::Ambiguous::tied` documents "lowest first". A caller's
        // candidate order must not change the verdict, and for equal scores the
        // sort alone cannot guarantee it, so the set is ordered explicitly.
        let ascending = measured(&[(0, 0.80), (1, 0.80), (2, 0.80)]);
        let descending = measured(&[(2, 0.80), (1, 0.80), (0, 0.80)]);
        let expected = Resolution::Ambiguous {
            tied: vec![0, 1, 2],
            tier: MatchTier::Semantic,
            score: 0.80,
        };
        assert_eq!(decide(&ascending, 0.72, 0.05), expected);
        assert_eq!(decide(&descending, 0.72, 0.05), expected);
    }

    #[test]
    fn permuting_the_option_list_does_not_change_the_verdict() {
        // End to end through the resolver, where a permutation means the same
        // options in a different order. The winner is the same option, and the
        // tied set is the same set.
        let resolver = LanguageResolver::new();
        let forwards = vec![
            String::from("Accept the offer"),
            String::from("Accept the order"),
        ];
        let backwards = vec![
            String::from("Accept the order"),
            String::from("Accept the offer"),
        ];
        assert_eq!(
            resolver.resolve("accept the", &ask(&forwards)),
            resolver.resolve("accept the", &ask(&backwards)),
            "the same options in a different order must reach the same verdict"
        );
    }

    #[test]
    fn every_option_is_measured_even_when_none_clears_the_threshold() {
        // Neither option is a candidate: "yes" is not "Yes, continue" and not
        // "No, go back" by any tier that accepts. Both were measured anyway, so
        // the reported `best_score` is the closest proximity actually observed
        // rather than the `0.0` a resolver that never ran would report.
        let choices = vec![String::from("Yes, continue"), String::from("No, go back")];
        let resolution = LanguageResolver::new().resolve("yes", &ask(&choices));
        assert!(
            matches!(
                resolution.resolution(),
                Resolution::Absent { best_score }
                    if *best_score > 0.0 && *best_score < MATCH_THRESHOLD
            ),
            "expected Absent at the best measured score below the threshold, got {resolution:?}"
        );
    }
}
