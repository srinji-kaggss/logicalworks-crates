//! Seeded deterministic simulation of the language resolver's contracts.
//!
//! The unit tests in `language/tests.rs` pin named cases; this sweep draws the
//! cases from a seed, so the properties hold across the input family rather than
//! across the examples someone thought of. Every family sweeps every band of the
//! seed space, and each band replays twice to one trace hash.
//!
//! | Family | Property, drawn per seed |
//! |---|---|
//! | `permutation_invariant` | a noisy exact answer resolves exactly to its option, and permuting the options never changes which *text* is chosen or tied |
//! | `normalize_folds_noise` | `normalize` is idempotent on arbitrary scalars, and case, punctuation and spacing noise fold away |
//! | `exported_table_reloads` | an exported alias table reloads into a resolver that answers every probe identically |
//! | `learn_then_forget_restores` | learning a fresh alias and forgetting it leaves every verdict exactly as before |
//! | `aliases_stay_in_their_question` | an alias learned in one question never changes another question's resolution |
//!
//! Sources: tier precedence and the alias scope are #25's counterexamples
//! (`"the usual"` resolving to `"Delete account"` in another question); the
//! migration and revocation rows are the alias table's documented contract in
//! `language.rs`. Real code throughout: the shipped `LanguageResolver`, no copy.

use crate::sim;

use std::error::Error;

use lgwks_bot::language::normalize;
use lgwks_bot::{Alias, LanguageResolver, Question, Resolution, Resolver, Verdict};

use sim::{Band, Rng};

/// A test that needs a parsed value returns `Result` and propagates, because the
/// crate forbids a panicking path anywhere, tests included.
pub type TestResult = Result<(), Box<dyn Error>>;

/// The words options and utterances are built from: distinct, lowercase, and
/// single-spaced once joined, so two distinct phrases never normalize alike.
const WORDS: [&str; 16] = [
    "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel", "india", "juliet",
    "kilo", "lima", "mike", "november", "oscar", "papa",
];

/// Words no option or shipped alias is built from, so an utterance drawn from
/// them is guaranteed unbound when a family learns it.
const FRESH_WORDS: [&str; 6] = ["quartz", "rumba", "sierra", "tango", "umbra", "vortex"];

/// The question identities the alias families spread their rows across.
const QUESTIONS: [&str; 3] = ["ask_one", "ask_two", "ask_three"];

/// Scalars the `normalize` family draws from: ASCII, accents, ligatures,
/// punctuation, spacing and a script the folding passes through.
const SCALARS: [char; 24] = [
    'a', 'B', 'z', 'Q', '7', ' ', ' ', '\t', ',', '!', '?', '.', '-', '\'', 'é', 'É', 'ß', 'œ',
    'Œ', 'ñ', 'ü', '日', '本', '😀',
];

/// Punctuation appended to a word as noise.
const TRAILING: [&str; 4] = [",", "!", "?", "."];

/// A draw below `bound`, as the index it will be used as.
fn draw(rng: &mut Rng, bound: usize) -> Result<usize, Box<dyn Error>> {
    Ok(usize::try_from(rng.below(u32::try_from(bound)?))?)
}

/// One element of `table`, drawn from the run's stream.
fn pick<'a, T>(rng: &mut Rng, table: &'a [T]) -> Result<&'a T, Box<dyn Error>> {
    let index = draw(rng, table.len())?;
    Ok(table
        .get(index)
        .ok_or("a draw below the length names an entry")?)
}

/// A phrase of one to three words from `words`, single-spaced and lowercase.
fn phrase(rng: &mut Rng, words: &[&str]) -> Result<String, Box<dyn Error>> {
    let count = draw(rng, 3)?.saturating_add(1);
    let mut parts = Vec::with_capacity(count);
    for _ in 0..count {
        parts.push(*pick(rng, words)?);
    }
    Ok(parts.join(" "))
}

/// Three to eight distinct options.
fn options(rng: &mut Rng) -> Result<Vec<String>, Box<dyn Error>> {
    let want = draw(rng, 6)?.saturating_add(3);
    let mut chosen: Vec<String> = Vec::with_capacity(want);
    // Bounded: the vocabulary holds far more than eight distinct phrases, and a
    // repeated draw is skipped rather than retried forever.
    for _ in 0..want.saturating_mul(8) {
        if chosen.len() == want {
            break;
        }
        let candidate = phrase(rng, &WORDS)?;
        if !chosen.contains(&candidate) {
            chosen.push(candidate);
        }
    }
    Ok(chosen)
}

/// `options` in a seeded order (Fisher-Yates over the run's stream).
fn shuffled(rng: &mut Rng, options: &[String]) -> Result<Vec<String>, Box<dyn Error>> {
    let mut out = options.to_vec();
    for top in (1..out.len()).rev() {
        let other = draw(rng, top.saturating_add(1))?;
        out.swap(top, other);
    }
    Ok(out)
}

/// `text` as a person might type it: case changed, punctuation after words,
/// and spacing widened, none of which changes what it normalizes to.
fn noisy(rng: &mut Rng, text: &str) -> Result<String, Box<dyn Error>> {
    let mut words = Vec::new();
    for word in text.split(' ') {
        let mut typed = match draw(rng, 3)? {
            0 => word.to_uppercase(),
            1 => {
                let mut scalars = word.chars();
                match scalars.next() {
                    Some(first) => first.to_uppercase().chain(scalars).collect(),
                    None => String::new(),
                }
            }
            _ => word.to_owned(),
        };
        if draw(rng, 3)? == 0 {
            typed.push_str(pick(rng, &TRAILING)?);
        }
        words.push(typed);
    }
    let gap = " ".repeat(draw(rng, 3)?.saturating_add(1));
    let lead = " ".repeat(draw(rng, 3)?);
    Ok(format!("{lead}{}{lead}", words.join(&gap)))
}

/// The option a verdict names, by text rather than position.
fn text_at(options: &[String], index: usize) -> Result<&str, Box<dyn Error>> {
    Ok(options
        .get(index)
        .map(String::as_str)
        .ok_or("a verdict named an option index past the list")?)
}

/// What a verdict decided, in terms that do not depend on option order.
///
/// Scores are left out on purpose: a fuzzy score summed in another order can
/// differ in its last bit, and what a permutation must not change is the
/// decision, which is the text chosen or tied and the tier it was decided in.
fn decision(verdict: &Verdict, options: &[String]) -> Result<String, Box<dyn Error>> {
    Ok(match *verdict.resolution() {
        Resolution::Resolved {
            index, ref tier, ..
        } => {
            format!("resolved {:?} at {tier:?}", text_at(options, index)?)
        }
        Resolution::Ambiguous {
            ref tied, ref tier, ..
        } => {
            let mut texts = Vec::with_capacity(tied.len());
            for index in tied {
                texts.push(text_at(options, *index)?);
            }
            texts.sort_unstable();
            format!("tied {texts:?} at {tier:?}")
        }
        Resolution::Absent { .. } => String::from("absent"),
        Resolution::StaleAlias {
            ref question,
            ref option,
        } => format!("stale {question} {option}"),
        ref other => format!("{other:?}"),
    })
}

/// Probe utterances for the alias families: every learned utterance (typed
/// noisily), every option (typed noisily), and a few phrases nobody bound.
fn probes(
    rng: &mut Rng,
    learned: &[Alias],
    options: &[String],
) -> Result<Vec<String>, Box<dyn Error>> {
    let mut out = Vec::new();
    for alias in learned {
        out.push(noisy(rng, alias.utterance())?);
    }
    for option in options {
        out.push(noisy(rng, option)?);
    }
    for _ in 0..4 {
        out.push(phrase(rng, &WORDS)?);
    }
    Ok(out)
}

/// One to `most` alias rows over the seed's questions and `options`.
fn alias_rows(
    rng: &mut Rng,
    options: &[String],
    most: usize,
) -> Result<Vec<Alias>, Box<dyn Error>> {
    let count = draw(rng, most)?.saturating_add(1);
    let mut rows = Vec::with_capacity(count);
    for _ in 0..count {
        let question = *pick(rng, &QUESTIONS)?;
        let utterance = phrase(rng, &WORDS)?;
        let option = pick(rng, options)?;
        rows.push(Alias::new(question, &utterance, option));
    }
    Ok(rows)
}

/// A noisy exact answer resolves exactly to its option, and no permutation of
/// the options changes the decision for it or for an arbitrary phrase.
fn permutation_invariant(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let resolver = LanguageResolver::new();
        for _ in 0..24 {
            let options = options(sim.rng())?;
            let chosen = pick(sim.rng(), &options)?.clone();
            let typed = noisy(sim.rng(), &chosen)?;
            let verdict = resolver.resolve(&typed, &Question::new("ask", &options));
            let expected = format!("resolved {chosen:?} at Exact");
            let observed = decision(&verdict, &options)?;
            if observed != expected {
                return Err(format!(
                    "seed {}: {typed:?} among {options:?} must resolve exactly to \
                     {chosen:?}, got {observed}",
                    sim.seed
                )
                .into());
            }
            let probe = phrase(sim.rng(), &WORDS)?;
            let permuted = shuffled(sim.rng(), &options)?;
            for utterance in [typed.as_str(), probe.as_str()] {
                let first = resolver.resolve(utterance, &Question::new("ask", &options));
                let second = resolver.resolve(utterance, &Question::new("ask", &permuted));
                let decided = decision(&first, &options)?;
                assert_eq!(
                    decided,
                    decision(&second, &permuted)?,
                    "seed {}: {utterance:?} decided differently under {options:?} and \
                     its permutation {permuted:?}",
                    sim.seed
                );
                sim.record(&decided);
            }
        }
        Ok(())
    })
}

/// `normalize` is idempotent on arbitrary scalars, and the noise a person adds
/// to a lowercase single-spaced phrase folds back to the phrase itself.
fn normalize_folds_noise(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        for _ in 0..48 {
            let length = draw(sim.rng(), 24)?;
            let mut raw = String::with_capacity(length);
            for _ in 0..length {
                raw.push(*pick(sim.rng(), &SCALARS)?);
            }
            let once = normalize(&raw);
            assert_eq!(
                normalize(&once),
                once,
                "seed {}: normalize must be idempotent, and {raw:?} folded to {once:?} \
                 which folds again",
                sim.seed
            );
            sim.record(&once);

            let clean = phrase(sim.rng(), &WORDS)?;
            let typed = noisy(sim.rng(), &clean)?;
            assert_eq!(
                normalize(&typed),
                clean,
                "seed {}: {typed:?} is {clean:?} with case, punctuation and spacing noise",
                sim.seed
            );
        }
        Ok(())
    })
}

/// An exported alias table reloads into a resolver that answers every probe,
/// in every question, exactly as the one it came from.
fn exported_table_reloads(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let options = options(sim.rng())?;
        let mut original = LanguageResolver::new();
        for row in alias_rows(sim.rng(), &options, 12)? {
            original.learn(row.question(), row.utterance(), row.option());
        }
        let table = original.aliases();
        let reloaded = LanguageResolver::with_aliases(table.clone());
        assert_eq!(
            reloaded.learned(),
            original.learned(),
            "seed {}: every exported row reloads",
            sim.seed
        );
        for utterance in probes(sim.rng(), &table, &options)? {
            for question in QUESTIONS {
                let asked = Question::new(question, &options);
                let before = original.resolve(&utterance, &asked);
                assert_eq!(
                    reloaded.resolve(&utterance, &asked),
                    before,
                    "seed {}: {utterance:?} in {question} answered differently after \
                     the table was exported and reloaded",
                    sim.seed
                );
                sim.record(&decision(&before, &options)?);
            }
        }
        sim.trace.record_number("rows", table.len());
        Ok(())
    })
}

/// Learning a fresh alias and forgetting it again leaves every verdict, in
/// every question, exactly as it was, and the forget returns the row learned.
fn learn_then_forget_restores(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let options = options(sim.rng())?;
        let shipped = alias_rows(sim.rng(), &options, 6)?;
        let mut resolver = LanguageResolver::with_aliases(shipped.clone());
        let probes = probes(sim.rng(), &shipped, &options)?;
        let baseline = |resolver: &LanguageResolver| -> Vec<Verdict> {
            let mut out = Vec::new();
            for utterance in &probes {
                for question in QUESTIONS {
                    out.push(resolver.resolve(utterance, &Question::new(question, &options)));
                }
            }
            out
        };
        let before = baseline(&resolver);
        let rows_before = resolver.learned();

        let question = *pick(sim.rng(), &QUESTIONS)?;
        let utterance = phrase(sim.rng(), &FRESH_WORDS)?;
        let option = pick(sim.rng(), &options)?.clone();
        assert!(
            resolver.learn(question, &utterance, &option).is_none(),
            "seed {}: {utterance:?} is drawn from words nothing was bound with",
            sim.seed
        );
        let learned = resolver.resolve(&utterance, &Question::new(question, &options));
        let revoked = resolver.forget(question, &utterance);
        assert_eq!(
            revoked.as_ref().map(Alias::option),
            Some(option.as_str()),
            "seed {}: forgetting returns the row that was learned",
            sim.seed
        );
        assert_eq!(
            resolver.learned(),
            rows_before,
            "seed {}: one row in, one out",
            sim.seed
        );
        assert_eq!(
            baseline(&resolver),
            before,
            "seed {}: learning {utterance:?} in {question} and forgetting it must leave \
             every verdict as it was",
            sim.seed
        );
        sim.record(&decision(&learned, &options)?);
        Ok(())
    })
}

/// An alias learned in one question never changes how another question reads
/// the same words over the same options (#25's cross-question counterexample).
fn aliases_stay_in_their_question(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let options = options(sim.rng())?;
        let mut taught = LanguageResolver::new();
        let mut rows = Vec::new();
        for row in alias_rows(sim.rng(), &options, 8)? {
            taught.learn(QUESTIONS[0], row.utterance(), row.option());
            rows.push(row);
        }
        let fresh = LanguageResolver::new();
        for utterance in probes(sim.rng(), &rows, &options)? {
            for question in &QUESTIONS[1..] {
                let asked = Question::new(question, &options);
                let isolated = decision(&fresh.resolve(&utterance, &asked), &options)?;
                assert_eq!(
                    decision(&taught.resolve(&utterance, &asked), &options)?,
                    isolated,
                    "seed {}: {utterance:?} in {question} was changed by aliases \
                     learned only in {}",
                    sim.seed,
                    QUESTIONS[0]
                );
                sim.record(&isolated);
            }
        }
        Ok(())
    })
}

/// Declares one test per band of the seed space for `$family`, so each family
/// sweeps every seed rather than a sample of them.
macro_rules! every_band {
    ($family:path) => {
        // The band-declaration macro, defined once for the whole layer.
        use crate::band_family::band_family;
        use super::{TestResult, sim};

        band_family!(
            r00 => $family, 0; r01 => $family, 1; r02 => $family, 2; r03 => $family, 3;
            r04 => $family, 4; r05 => $family, 5; r06 => $family, 6; r07 => $family, 7;
            r08 => $family, 8; r09 => $family, 9; r10 => $family, 10; r11 => $family, 11;
            r12 => $family, 12; r13 => $family, 13; r14 => $family, 14; r15 => $family, 15;
            r16 => $family, 16; r17 => $family, 17; r18 => $family, 18; r19 => $family, 19;
            r20 => $family, 20; r21 => $family, 21; r22 => $family, 22; r23 => $family, 23;
            r24 => $family, 24; r25 => $family, 25; r26 => $family, 26; r27 => $family, 27;
            r28 => $family, 28; r29 => $family, 29; r30 => $family, 30; r31 => $family, 31;
        );
    };
}

mod permutation {
    every_band!(super::permutation_invariant);
}

mod normalization {
    every_band!(super::normalize_folds_noise);
}

mod migration {
    every_band!(super::exported_table_reloads);
}

mod revocation {
    every_band!(super::learn_then_forget_restores);
}

mod scope {
    every_band!(super::aliases_stay_in_their_question);
}
