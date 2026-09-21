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

use crate::session::{MatchTier, Resolution};

/// The longest normalized input the shipped resolver will compare.
pub const MAX_UTTERANCE_CHARS: usize = 512;

/// The token-overlap weight inside the fuzzy tier.
const TOKEN_WEIGHT: f64 = 0.5;

/// The edit-distance weight inside the fuzzy tier.
const DISTANCE_WEIGHT: f64 = 0.5;

/// The score at or above which a fuzzy match is a candidate at all.
pub const MATCH_THRESHOLD: f64 = 0.55;

/// The lead the best candidate must hold over the runner-up.
pub const MATCH_MARGIN: f64 = 0.08;

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

/// Scores every option candidate against one utterance, best-first.
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
    options: &[String],
    aliases: &BTreeMap<String, usize>,
    distance: &EditDistance,
) -> Vec<(usize, MatchTier, f64)> {
    let spoken = normalize(utterance);
    let spoken_key = phonetic_key(utterance);
    let spoken_tokens = tokens(utterance);
    let overlap = Jaccard::<String>::new();

    let mut scored: Vec<(usize, MatchTier, f64)> = Vec::with_capacity(options.len());
    for (index, option) in options.iter().enumerate() {
        let canonical = normalize(option);
        let tier_and_score = if aliases.get(&spoken).copied() == Some(index) {
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
    Resolution::Ambiguous { tied, score }
}

/// The shipped lexicon resolver: tiered matching plus learned aliases.
///
/// Deliberately not `Debug`: the alias table is the interesting state and a
/// derived `Debug` would print it in whatever order the map iterates.
#[derive(Clone)]
#[non_exhaustive]
pub struct LanguageResolver {
    /// Learned utterance-to-option aliases, keyed by normalized utterance.
    aliases: BTreeMap<String, usize>,
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
    /// house vocabulary as data, and a reviewer can read the whole of it.
    #[must_use]
    pub fn with_aliases(aliases: Vec<(String, usize)>) -> Self {
        let mut resolver = Self::new();
        for (utterance, index) in aliases {
            resolver.aliases.insert(normalize(&utterance), index);
        }
        resolver
    }

    /// Records that `utterance` means the option at `index`.
    ///
    /// This is the vocabulary axis of linguistic change: the caller calls it
    /// when a resolution was confirmed, so next time the phrase is exact rather
    /// than a fuzzy guess. A row is readable and revocable, which a fitted
    /// weight is not. Returns the previous binding, if the phrase was already
    /// learned — so re-teaching a phrase is a visible change, not a silent one.
    pub fn learn(&mut self, utterance: &str, index: usize) -> Option<usize> {
        self.aliases.insert(normalize(utterance), index)
    }

    /// Forgets a learned alias, returning whether one was present.
    pub fn forget(&mut self, utterance: &str) -> bool {
        self.aliases.remove(&normalize(utterance)).is_some()
    }

    /// Returns the number of learned aliases.
    #[must_use]
    pub fn learned(&self) -> usize {
        self.aliases.len()
    }

    /// Resolves `utterance` against `options` as a three-way verdict.
    #[must_use]
    pub fn decide_for(&self, utterance: &str, options: &[String]) -> Resolution {
        decide(
            &score_all(utterance, options, &self.aliases, &self.distance),
            MATCH_THRESHOLD,
            MATCH_MARGIN,
        )
    }
}

impl Default for LanguageResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::session::Resolver for LanguageResolver {
    fn resolve(&self, utterance: &str, options: &[String]) -> Resolution {
        self.decide_for(utterance, options)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Resolver;

    /// Builds the option list most tests resolve against.
    fn options() -> Vec<String> {
        vec![
            String::from("Yes, continue"),
            String::from("No, go back"),
            String::from("Speak to a person"),
        ]
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
        let resolution = LanguageResolver::new().resolve("  yes, CONTINUE ", &options());
        // The runner-up here is "Yes, continue", which the fuzzy tier measures
        // at roughly `0.375` — below `MATCH_THRESHOLD`. It counted against the
        // lead anyway, so `lead` is now that measured gap and the old
        // `lead: 1.0` — the whole score reported as a margin over a competitor
        // deleted before it was compared — is gone.
        assert!(
            matches!(
                resolution,
                Resolution::Resolved {
                    index: 0,
                    tier: MatchTier::Exact,
                    score,
                    lead,
                } if (score - 1.0).abs() < 1e-9
                    && lead < score
                    && clears_the_margin(lead)
            ),
            "expected an exact resolution of option 0 leading a measured runner-up, got {resolution:?}"
        );
    }

    #[test]
    fn a_spelling_variation_resolves_at_the_phonetic_tier() {
        let choices = vec![String::from("Smyth"), String::from("Marcus")];
        let resolution = LanguageResolver::new().resolve("Smith", &choices);
        assert!(
            matches!(
                resolution,
                Resolution::Resolved {
                    index: 0,
                    tier: MatchTier::Phonetic,
                    score,
                    lead,
                } if (score - 0.9).abs() < 1e-9 && clears_the_margin(lead)
            ),
            "expected a phonetic resolution of option 0, got {resolution:?}"
        );
    }

    #[test]
    fn an_unrecognized_answer_is_absent() {
        let resolution = LanguageResolver::new().resolve("maybe later", &options());
        assert!(
            matches!(resolution, Resolution::Absent { .. }),
            "expected Absent, got {resolution:?}"
        );
    }

    #[test]
    fn two_equally_close_options_are_ambiguous_and_both_are_tied() {
        let choices = vec![
            String::from("Accept the offer"),
            String::from("Accept the order"),
        ];
        let resolution = LanguageResolver::new().resolve("accept the", &choices);
        match resolution {
            Resolution::Ambiguous { tied, .. } => {
                assert_eq!(tied, vec![0, 1], "both options must stay in play");
            }
            other => assert!(
                matches!(other, Resolution::Ambiguous { .. }),
                "expected Ambiguous, got {other:?}"
            ),
        }
    }

    #[test]
    fn a_learned_alias_resolves_exactly_and_is_revocable() {
        let mut resolver = LanguageResolver::new();
        assert_eq!(resolver.learned(), 0);
        assert_eq!(resolver.learn("the usual", 2), None, "a fresh alias");
        assert_eq!(resolver.learned(), 1);

        assert!(
            matches!(
                resolver.resolve("The usual!", &options()),
                Resolution::Resolved {
                    index: 2,
                    tier: MatchTier::Exact,
                    score,
                    lead,
                } if (score - 1.0).abs() < 1e-9 && clears_the_margin(lead)
            ),
            "an alias is a confirmed exact match, not a fuzzy guess"
        );

        assert_eq!(
            resolver.learn("the usual", 1),
            Some(2),
            "re-teaching is visible"
        );
        assert!(
            resolver.forget("the usual"),
            "forgetting reports the removal"
        );
        assert!(
            !resolver.forget("the usual"),
            "forgetting twice reports nothing"
        );
        assert_eq!(resolver.learned(), 0);
    }

    #[test]
    fn a_shipped_alias_table_is_normalized_on_load() {
        let resolver = LanguageResolver::with_aliases(vec![(String::from("  The Usual "), 2)]);
        assert!(
            matches!(
                resolver.resolve("the usual", &options()),
                Resolution::Resolved {
                    index: 2,
                    tier: MatchTier::Exact,
                    score,
                    lead,
                } if (score - 1.0).abs() < 1e-9 && clears_the_margin(lead)
            ),
            "the shipped table is normalized on load and the lead is a measured gap"
        );
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
        let resolution = LanguageResolver::new().resolve(&long, &options());
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
            LanguageResolver::new().resolve("yes", &[]),
            Resolution::Absent { best_score: 0.0 }
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
            resolver.resolve("accept the", &forwards),
            resolver.resolve("accept the", &backwards),
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
        let resolution = LanguageResolver::new().resolve("yes", &choices);
        assert!(
            matches!(
                resolution,
                Resolution::Absent { best_score }
                    if best_score > 0.0 && best_score < MATCH_THRESHOLD
            ),
            "expected Absent at the best measured score below the threshold, got {resolution:?}"
        );
    }
}
