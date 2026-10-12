//! Seeded simulation of the shipped lexicon resolver.
//!
//! One seed draws option lists, utterances (options re-cased, re-punctuated,
//! accented and misspelled, plus phrases no option resembles) and learned
//! aliases, and every family asserts what a person answering a question sees:
//!
//! - normalizing is idempotent and yields lower-case words single-spaced;
//! - a phonetic key is empty or one upper-case letter and three digits, and
//!   folding case or accents does not change it;
//! - an option typed in any case or punctuation resolves to itself, exactly,
//!   wherever it sits in the list;
//! - a learned phrase resolves exactly in its own question and is spent
//!   nowhere else; a superseded one is never a silent `Absent`;
//! - forgetting, exporting and reloading the alias table keep every verdict;
//! - a numeric question keeps the sign, refuses what is not offered, and ties
//!   two spellings of one number;
//! - every `Resolved` cleared the threshold by the margin, and every
//!   `Ambiguous` lists the tied in ascending order;
//! - the same seed replays the same verdicts and distinct seeds diverge.
//!
//! Sources: Soundex (Russell and Odell, US patent 1,261,167, 1918), the
//! Levenshtein distance the fuzzy tier bounds, and RFC 5198's guidance that
//! text compared across systems is compared in one normalized form.

use crate::language::{Alias, LanguageResolver, MATCH_MARGIN, MATCH_THRESHOLD, normalize, phonetic_key};
use crate::session::{AnswerDomain, MatchTier, Question, Resolution};
use crate::sim_stream::{RECEIPT_START, Stream, fold};

/// The seeds every family replays.
const SEEDS: [u64; 4] = [
    0x1E81_0000_0000_0001,
    0x1E81_0000_0000_0002,
    0x1E81_FFFF_0000_0003,
    0xC0DA_1E81_0000_0004,
];

/// Questions each family draws per seed.
const DRAWS: usize = 96;

/// The options questions are drawn from, with near neighbours on purpose.
const OPTIONS: [&str; 14] = [
    "Repeat last order",
    "Cancel",
    "Smith",
    "Smyth",
    "Strasse",
    "yes",
    "no",
    "Catherine",
    "Kathryn",
    "Talk to a person",
    "Café crème",
    "Help",
    "Stop all deliveries",
    "Ship to my office",
];

/// Phrases a group coins that no option resembles.
const COINED: [&str; 6] = ["the usual", "same again", "nope nope", "zq", "x7", "lol k"];

/// A family's result.
type Outcome = Result<(), String>;

/// One drawn question: its id and two to six distinct options.
struct Drawn {
    /// The question id.
    id: String,
    /// The options, in the order offered.
    options: Vec<String>,
}

impl Drawn {
    /// A question drawn from `stream`.
    fn from_stream(stream: &mut Stream) -> Result<Self, String> {
        let count = stream.below(5)?.saturating_add(2);
        let mut options: Vec<String> = Vec::with_capacity(count);
        while options.len() < count {
            let option = stream.choose(&OPTIONS)?;
            if !options.iter().any(|held| held == option) {
                options.push(option.to_owned());
            }
        }
        Ok(Self {
            id: format!("q{}", stream.below(4)?),
            options,
        })
    }

    /// The question as the resolver reads it.
    fn question(&self) -> Question<'_> {
        Question::new(&self.id, &self.options)
    }

    /// A drawn option of this question.
    fn option(&self, stream: &mut Stream) -> Result<&str, String> {
        let at = stream.below(self.options.len())?;
        self.options
            .get(at)
            .map(String::as_str)
            .ok_or_else(|| format!("option {at} is not offered"))
    }
}

/// `text` re-cased and re-punctuated the way a person types it.
fn retyped(stream: &mut Stream, text: &str) -> Result<String, String> {
    let mut typed = String::with_capacity(text.len().saturating_add(4));
    if stream.below(3)? == 0 {
        typed.push_str("  ");
    }
    for glyph in text.chars() {
        match stream.below(3)? {
            0 => typed.extend(glyph.to_uppercase()),
            1 => typed.extend(glyph.to_lowercase()),
            _ => typed.push(glyph),
        }
        if glyph == ' ' && stream.below(2)? == 0 {
            typed.push_str(" ,");
        }
    }
    typed.push_str(stream.choose(&["", "!", "?", ".", " "])?);
    Ok(typed)
}

/// Every drawn question under each seed.
fn sweep(mut check: impl FnMut(u64, &mut Stream, &Drawn) -> Outcome) -> Outcome {
    for seed in SEEDS {
        let mut stream = Stream::new(seed);
        for _ in 0..DRAWS {
            let drawn = Drawn::from_stream(&mut stream)?;
            check(seed, &mut stream, &drawn)?;
        }
    }
    Ok(())
}

#[test]
/// Normalizing is idempotent and yields lower-case words single-spaced.
fn sim_normalizing_is_idempotent_and_single_spaced() -> Outcome {
    sweep(|seed, stream, drawn| {
        for option in &drawn.options {
            let typed = retyped(stream, option)?;
            let once = normalize(&typed);
            assert_eq!(normalize(&once), once, "seed {seed:#x}: {typed:?}");
            assert!(
                !once.starts_with(' ') && !once.ends_with(' ') && !once.contains("  "),
                "seed {seed:#x}: {once:?} is single-spaced"
            );
            assert!(
                once.chars().all(|glyph| glyph == ' ' || (glyph.is_alphanumeric() && !glyph.is_uppercase())),
                "seed {seed:#x}: {once:?} is lower-case words"
            );
            assert_eq!(once, normalize(option), "seed {seed:#x}: retyping changes nothing");
        }
        Ok(())
    })
}

#[test]
/// A phonetic key is empty or a letter and three digits, and folding case or
/// accents does not change it.
fn sim_a_phonetic_key_is_soundex_shaped_and_fold_invariant() -> Outcome {
    sweep(|seed, stream, drawn| {
        for option in drawn.options.iter().map(String::as_str).chain(COINED) {
            let key = phonetic_key(option);
            let mut glyphs = key.chars();
            let shaped = key.is_empty()
                || (key.chars().count() == 4
                    && glyphs.next().is_some_and(|first| first.is_ascii_uppercase())
                    && glyphs.all(|digit| digit.is_ascii_digit()));
            assert!(shaped, "seed {seed:#x}: {option:?} keyed {key:?}");
            let typed = retyped(stream, option)?;
            assert_eq!(phonetic_key(&typed), key, "seed {seed:#x}: {typed:?}");
            assert_eq!(phonetic_key(&normalize(option)), key, "seed {seed:#x}: folded");
        }
        Ok(())
    })
}

#[test]
/// An option typed in any case or punctuation resolves to itself, exactly.
fn sim_a_retyped_option_resolves_exactly_to_itself() -> Outcome {
    sweep(|seed, stream, drawn| {
        let resolver = LanguageResolver::new();
        let at = stream.below(drawn.options.len())?;
        let option = drawn.options.get(at).ok_or("the drawn option exists")?;
        let typed = retyped(stream, option)?;
        match resolver.decide_for(&typed, &drawn.question()) {
            Resolution::Resolved { index, tier, .. } => {
                assert_eq!(index, at, "seed {seed:#x}: {typed:?} chose another option");
                assert_eq!(tier, MatchTier::Exact, "seed {seed:#x}: {typed:?}");
                Ok(())
            }
            other => Err(format!("seed {seed:#x}: {typed:?} in {:?} gave {other:?}", drawn.options)),
        }
    })
}

#[test]
/// A learned phrase resolves exactly in its own question, and nowhere else.
fn sim_a_learned_phrase_is_spent_only_in_its_own_question() -> Outcome {
    sweep(|seed, stream, drawn| {
        let mut resolver = LanguageResolver::new();
        let phrase = stream.choose(&COINED)?;
        let at = stream.below(drawn.options.len())?;
        let option = drawn.options.get(at).ok_or("the drawn option exists")?;
        resolver.learn(&drawn.id, phrase, option);
        match resolver.decide_for(phrase, &drawn.question()) {
            Resolution::Resolved { index, tier, .. } => {
                assert_eq!((index, tier), (at, MatchTier::Exact), "seed {seed:#x}");
            }
            other => return Err(format!("seed {seed:#x}: {phrase:?} gave {other:?}")),
        }
        let elsewhere = format!("{}-other", drawn.id);
        let other_question = Question::new(&elsewhere, &drawn.options);
        let spent = matches!(
            resolver.decide_for(phrase, &other_question),
            Resolution::Resolved { tier: MatchTier::Exact, .. }
        );
        assert!(!spent, "seed {seed:#x}: {phrase:?} was spent in another question");
        Ok(())
    })
}

#[test]
/// A phrase whose learned option was withdrawn is never a silent `Absent`.
fn sim_a_superseded_phrase_is_never_silently_absent() -> Outcome {
    sweep(|seed, stream, drawn| {
        let phrase = stream.choose(&COINED)?;
        let withdrawn = stream.choose(&OPTIONS)?;
        if drawn.options.iter().any(|option| option == withdrawn) {
            return Ok(());
        }
        let resolver =
            LanguageResolver::with_aliases(vec![Alias::new(&drawn.id, phrase, withdrawn)]);
        match resolver.decide_for(phrase, &drawn.question()) {
            Resolution::Absent { .. } => {
                Err(format!("seed {seed:#x}: {phrase:?} lost its withdrawn {withdrawn:?}"))
            }
            Resolution::StaleAlias { question, option } => {
                assert_eq!((question.as_str(), option.as_str()), (drawn.id.as_str(), withdrawn));
                Ok(())
            }
            _ => Ok(()),
        }
    })
}

#[test]
/// Exporting and reloading the alias table keeps every verdict, and
/// forgetting a phrase removes exactly its row.
fn sim_the_alias_table_round_trips_and_forgets_one_row() -> Outcome {
    sweep(|seed, stream, drawn| {
        let mut resolver = LanguageResolver::new();
        for phrase in COINED {
            let option = drawn.option(stream)?;
            resolver.learn(&drawn.id, phrase, option);
        }
        let reloaded = LanguageResolver::with_aliases(resolver.aliases());
        assert_eq!(reloaded.learned(), resolver.learned(), "seed {seed:#x}");
        for phrase in COINED {
            assert_eq!(
                format!("{:?}", reloaded.decide_for(phrase, &drawn.question())),
                format!("{:?}", resolver.decide_for(phrase, &drawn.question())),
                "seed {seed:#x}: {phrase:?}"
            );
        }
        let before = resolver.learned();
        let forgotten = stream.choose(&COINED)?;
        let removed = resolver.forget(&drawn.id, forgotten);
        assert_eq!(
            removed.map(|alias| alias.utterance().to_owned()),
            Some(normalize(forgotten)),
            "seed {seed:#x}: the row forgotten is the phrase's"
        );
        assert_eq!(resolver.learned(), before.saturating_sub(1), "seed {seed:#x}");
        assert!(resolver.forget(&drawn.id, forgotten).is_none(), "seed {seed:#x}: once");
        Ok(())
    })
}

#[test]
/// A numeric question keeps the sign, refuses what is not offered, and ties
/// two spellings of one number.
fn sim_a_numeric_question_reads_the_value_with_its_sign() -> Outcome {
    for seed in SEEDS {
        let mut stream = Stream::new(seed);
        for _ in 0..DRAWS {
            let value = i64::try_from(stream.below(1_000)?).map_err(|error| error.to_string())?;
            let options = vec![value.to_string(), (-value).to_string(), format!("0{value}")];
            let question = Question::new("count", &options).with_domain(AnswerDomain::Integer);
            let resolver = LanguageResolver::new();
            if value != 0 {
                match resolver.decide_for(&format!("-{value}"), &question) {
                    Resolution::Resolved { index: 1, .. } => {}
                    other => return Err(format!("seed {seed:#x}: -{value} gave {other:?}")),
                }
            }
            match resolver.decide_for(&value.to_string(), &question) {
                Resolution::Ambiguous { tied, .. } if value != 0 => {
                    assert_eq!(tied, vec![0, 2], "seed {seed:#x}: {value} and 0{value} tie");
                }
                Resolution::Ambiguous { tied, .. } => {
                    assert_eq!(tied, vec![0, 1, 2], "seed {seed:#x}: three spellings of 0");
                }
                other => return Err(format!("seed {seed:#x}: {value} gave {other:?}")),
            }
            let absent = value.saturating_add(1_000).to_string();
            assert!(
                matches!(resolver.decide_for(&absent, &question), Resolution::Absent { .. }),
                "seed {seed:#x}: {absent} is not offered"
            );
        }
    }
    Ok(())
}

#[test]
/// Every `Resolved` cleared the threshold by the margin, and every
/// `Ambiguous` lists the tied in ascending order.
fn sim_every_verdict_honours_the_threshold_and_the_margin() -> Outcome {
    sweep(|seed, stream, drawn| {
        let resolver = LanguageResolver::new();
        let utterance = match stream.below(3)? {
            0 => stream.choose(&COINED)?.to_owned(),
            1 => {
                let option = drawn.option(stream)?;
                let cut = option.chars().count().saturating_sub(1);
                option.chars().take(cut).collect()
            }
            _ => {
                let option = stream.choose(&OPTIONS)?;
                retyped(stream, option)?
            }
        };
        match resolver.decide_for(&utterance, &drawn.question()) {
            Resolution::Resolved { score, lead, .. } => {
                assert!(score >= MATCH_THRESHOLD, "seed {seed:#x}: {utterance:?} at {score}");
                assert!(lead >= MATCH_MARGIN, "seed {seed:#x}: {utterance:?} led by {lead}");
            }
            Resolution::Ambiguous { tied, .. } => {
                assert!(tied.len() >= 2, "seed {seed:#x}: a tie of one");
                assert!(tied.windows(2).all(|pair| pair.first() < pair.get(1)), "seed {seed:#x}");
            }
            Resolution::Absent { best_score } => {
                assert!(best_score < MATCH_THRESHOLD, "seed {seed:#x}: absent at {best_score}");
            }
            _ => {}
        }
        Ok(())
    })
}

/// The replay receipt of every verdict drawn under `seed`.
fn verdict_trace(seed: u64) -> Result<u64, String> {
    let mut stream = Stream::new(seed);
    let resolver = LanguageResolver::new();
    let mut trace = RECEIPT_START;
    for _ in 0..DRAWS {
        let drawn = Drawn::from_stream(&mut stream)?;
        let option = stream.choose(&OPTIONS)?;
        let utterance = retyped(&mut stream, option)?;
        let verdict = resolver.decide_for(&utterance, &drawn.question());
        trace = fold(trace, format!("{utterance}={verdict:?};").as_bytes());
    }
    Ok(trace)
}

#[test]
/// The replay oracle: one seed, one verdict trace.
fn sim_the_same_seed_replays_the_same_verdicts() -> Outcome {
    for seed in SEEDS {
        assert_eq!(verdict_trace(seed)?, verdict_trace(seed)?, "seed {seed:#x} replays");
    }
    Ok(())
}

#[test]
/// The divergence oracle: a sweep that ignored its seed would pass the replay.
fn sim_distinct_seeds_draw_different_verdicts() -> Outcome {
    assert_ne!(
        verdict_trace(SEEDS[0])?,
        verdict_trace(SEEDS[1])?,
        "two seeds drew alike"
    );
    Ok(())
}
