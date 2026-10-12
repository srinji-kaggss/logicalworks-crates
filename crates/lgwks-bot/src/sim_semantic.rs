//! Seeded simulation of the semantic tier behind the lexicon.
//!
//! One seed draws option lists, utterances and a scripted embedder — one that
//! answers with chosen vectors, fails, returns the wrong width, or returns a
//! vector no angle can be drawn from — and every family asserts what a caller
//! routing on the verdict sees:
//!
//! - an identity and a policy are refused in their documented order and held
//!   exactly when admitted;
//! - whatever the lexicon decides, other than `Absent`, is the verdict
//!   verbatim, and no model is named on it;
//! - a failing, mis-sized or unmeasurable embedding anywhere degrades the
//!   whole verdict, naming the model, and is never reported as `Absent`;
//! - an utterance aligned with one option resolves to it on the semantic
//!   tier, clearing threshold and margin; one split between two is `Absent`
//!   under the shipped policy and a two-way tie under a looser one;
//! - a numeric question and a learned phrase never reach the model;
//! - equal policies have one version and different ones differ;
//! - the same seed replays the same verdicts and distinct seeds diverge.
//!
//! Sources: Reimers and Gurevych 2019 (Sentence-BERT cosine similarity), the
//! all-or-nothing comparison rule in `semantic.rs`, and issue #31 (a winner's
//! lead measured against a field one member short).

use std::collections::BTreeMap;
use std::fmt;

use crate::language::LanguageResolver;
use crate::semantic::{Embedder, EmbedderIdentity, SemanticError, SemanticPolicy, SemanticResolver};
use crate::session::{AnswerDomain, DegradedReason, MatchTier, Question, Resolution, Resolver};
use crate::sim_stream::{RECEIPT_START, Stream, fold};

/// The seeds every family replays.
const SEEDS: [u64; 4] = [
    0x5E3A_0000_0000_0001,
    0x5E3A_0000_0000_0002,
    0x5E3A_FFFF_0000_0003,
    0xB0B5_5E3A_0000_0004,
];

/// Scenarios each family draws per seed.
const DRAWS: usize = 64;

/// The width every scripted vector has.
const WIDTH: usize = 8;

/// Options a question offers, none resembling a coined phrase.
const OPTIONS: [&str; 6] = [
    "Repeat last order",
    "Cancel",
    "Talk to a person",
    "Ship to my office",
    "Stop all deliveries",
    "Pause my plan",
];

/// Phrases no option resembles lexically.
const COINED: [&str; 4] = ["the usual", "zq", "x7 k", "lol"];

/// A family's result.
type Outcome = Result<(), String>;

/// The scripted embedder's one failure.
#[derive(Debug)]
struct Unavailable;

impl fmt::Display for Unavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("the scripted embedder is unavailable")
    }
}

impl std::error::Error for Unavailable {}

/// An embedder that answers from a table; text the table lacks fails.
struct Scripted {
    /// Who it says it is.
    identity: EmbedderIdentity,
    /// The vector for each text.
    table: BTreeMap<String, Vec<f32>>,
}

impl Embedder for Scripted {
    type Error = Unavailable;

    fn identity(&self) -> &EmbedderIdentity {
        &self.identity
    }

    fn embed(&self, text: &str) -> Result<Vec<f32>, Unavailable> {
        self.table.get(text).cloned().ok_or(Unavailable)
    }
}

/// The unit vector along `axis`.
fn basis(axis: usize) -> Vec<f32> {
    (0..WIDTH).map(|at| if at == axis { 1.0 } else { 0.0 }).collect()
}

/// One drawn scenario: options, a coined utterance and the option it means.
struct Scenario {
    /// The options, in the order offered.
    options: Vec<String>,
    /// The coined utterance.
    utterance: &'static str,
    /// The option the utterance means.
    target: usize,
    /// A second option for a split utterance.
    other: usize,
}

impl Scenario {
    /// A scenario drawn from `stream`.
    fn from_stream(stream: &mut Stream) -> Result<Self, String> {
        let count = stream.below(4)?.saturating_add(2);
        let start = stream.below(OPTIONS.len())?;
        let options: Vec<String> = (0..count)
            .map(|offset| {
                start
                    .saturating_add(offset)
                    .checked_rem(OPTIONS.len())
                    .and_then(|at| OPTIONS.get(at))
                    .map(|text| (*text).to_owned())
                    .ok_or("a pooled option")
            })
            .collect::<Result<_, _>>()?;
        let target = stream.below(count)?;
        let other = target
            .saturating_add(1)
            .checked_rem(count)
            .ok_or("a question offers options")?;
        Ok(Self {
            options,
            utterance: stream.choose(&COINED)?,
            target,
            other,
        })
    }

    /// The question these options make.
    fn question(&self) -> Question<'_> {
        Question::new("sim", &self.options)
    }

    /// An embedder giving each option its own axis and the utterance `spoken`.
    fn embedder(&self, spoken: Vec<f32>) -> Result<Scripted, String> {
        let mut table: BTreeMap<String, Vec<f32>> = self
            .options
            .iter()
            .enumerate()
            .map(|(axis, option)| (option.clone(), basis(axis)))
            .collect();
        table.insert(self.utterance.to_owned(), spoken);
        Ok(Scripted {
            identity: EmbedderIdentity::new("sim-embedder", "sha256:5e3a", WIDTH)
                .map_err(|error| error.to_string())?,
            table,
        })
    }

    /// Whether the lexicon finds nothing for the utterance, so the model is
    /// consulted.
    fn lexicon_absent(&self) -> bool {
        matches!(
            LanguageResolver::new().decide_for(self.utterance, &self.question()),
            Resolution::Absent { .. }
        )
    }
}

/// Every drawn scenario under each seed.
fn sweep(check: impl FnMut(u64, &mut Stream, &Scenario) -> Outcome) -> Outcome {
    crate::sim_stream::sweep(&SEEDS, DRAWS, Scenario::from_stream, check)
}

#[test]
/// An identity is refused unnamed, then undigested, then zero-width, and held
/// exactly when admitted.
fn sim_an_identity_is_refused_in_order_and_held_exactly() -> Outcome {
    for seed in SEEDS {
        let mut stream = Stream::new(seed);
        for _ in 0..DRAWS {
            let name = stream.choose(&["", "MiniLM", "e5-small"])?;
            let digest = stream.choose(&["", "sha256:ab12"])?;
            let width = stream.below(3)?;
            let expected = if name.is_empty() {
                Err(SemanticError::UnnamedEmbedder)
            } else if digest.is_empty() {
                Err(SemanticError::UndigestedEmbedder { name: name.to_owned() })
            } else if width == 0 {
                Err(SemanticError::ZeroDimension { name: name.to_owned() })
            } else {
                Ok((name, digest, width))
            };
            let made = EmbedderIdentity::new(name, digest, width);
            match (made, expected) {
                (Ok(identity), Ok(fields)) => {
                    assert_eq!((identity.name(), identity.digest(), identity.dimension()), fields);
                }
                (Err(refusal), Err(expected)) => assert_eq!(refusal, expected, "seed {seed:#x}"),
                (made, expected) => {
                    return Err(format!("seed {seed:#x}: {made:?}, expected {expected:?}"));
                }
            }
        }
    }
    Ok(())
}

#[test]
/// A policy is refused for a non-finite or out-of-range threshold, then
/// margin, in that order, and held exactly when admitted.
fn sim_a_policy_is_refused_in_order_and_held_exactly() -> Outcome {
    let values = [f64::NAN, f64::INFINITY, -0.25, 0.0, 0.5, 0.72, 1.0, 1.5];
    for seed in SEEDS {
        let mut stream = Stream::new(seed);
        for _ in 0..DRAWS {
            let (threshold, margin) = (stream.choose(&values)?, stream.choose(&values)?);
            let in_range = |value: f64| (0.0..=1.0).contains(&value);
            match SemanticPolicy::new(threshold, margin) {
                Ok(policy) => {
                    assert!(in_range(threshold) && in_range(margin), "seed {seed:#x}");
                    assert_eq!((policy.threshold(), policy.margin()), (threshold, margin));
                }
                Err(SemanticError::NonFiniteThreshold) => assert!(!threshold.is_finite()),
                Err(SemanticError::InvalidThreshold { .. }) => {
                    assert!(threshold.is_finite() && !in_range(threshold), "seed {seed:#x}");
                }
                Err(SemanticError::NonFiniteMargin) => {
                    assert!(in_range(threshold) && !margin.is_finite(), "seed {seed:#x}");
                }
                Err(SemanticError::InvalidMargin { .. }) => {
                    assert!(in_range(threshold) && margin.is_finite() && !in_range(margin));
                }
                Err(other) => return Err(format!("seed {seed:#x}: {other}")),
            }
        }
    }
    Ok(())
}

#[test]
/// Whatever the lexicon decides other than `Absent` is the verdict verbatim,
/// with no model named, even over an embedder that would fail.
fn sim_the_lexicon_stays_authoritative() -> Outcome {
    sweep(|seed, stream, scenario| {
        let typed = stream.choose(&OPTIONS)?;
        let mut failing = scenario.embedder(basis(0))?;
        failing.table.clear();
        let resolver = SemanticResolver::new(failing);
        let lexicon = LanguageResolver::new().decide_for(typed, &scenario.question());
        let verdict = resolver.resolve(typed, &scenario.question());
        if !matches!(lexicon, Resolution::Absent { .. }) {
            assert_eq!(format!("{:?}", verdict.resolution()), format!("{lexicon:?}"));
            assert!(verdict.provenance().model().is_none(), "seed {seed:#x}: no model ran");
        }
        Ok(())
    })
}

#[test]
/// A failing or mis-sized embedding degrades the verdict as unavailable,
/// naming the model, never as `Absent`.
fn sim_a_failing_or_missized_embedder_degrades() -> Outcome {
    sweep(|seed, stream, scenario| {
        if !scenario.lexicon_absent() {
            return Ok(());
        }
        let mut embedder = scenario.embedder(basis(scenario.target))?;
        if stream.below(2)? == 0 {
            embedder.table.remove(scenario.utterance);
        } else {
            let at = stream.below(scenario.options.len())?;
            let option = scenario.options.get(at).ok_or("a drawn option")?;
            embedder.table.insert(option.clone(), vec![1.0; WIDTH.saturating_add(1)]);
        }
        let verdict = SemanticResolver::new(embedder).resolve(scenario.utterance, &scenario.question());
        assert!(
            matches!(
                verdict.resolution(),
                Resolution::Degraded { reason: DegradedReason::EmbedderUnavailable }
            ),
            "seed {seed:#x}: {:?}",
            verdict.resolution()
        );
        assert_eq!(verdict.provenance().model().map(EmbedderIdentity::name), Some("sim-embedder"));
        Ok(())
    })
}

#[test]
/// A zero or non-finite vector anywhere in the set degrades the whole verdict
/// as unmeasurable.
fn sim_an_unmeasurable_vector_anywhere_degrades_the_whole_verdict() -> Outcome {
    sweep(|seed, stream, scenario| {
        if !scenario.lexicon_absent() {
            return Ok(());
        }
        let degenerate = if stream.below(2)? == 0 {
            vec![0.0; WIDTH]
        } else {
            let mut poisoned = basis(0);
            if let Some(last) = poisoned.last_mut() {
                *last = f32::NAN;
            }
            poisoned
        };
        let mut embedder = scenario.embedder(basis(scenario.target))?;
        let victim = match stream.below(scenario.options.len().saturating_add(1))? {
            0 => scenario.utterance.to_owned(),
            at => scenario.options.get(at.saturating_sub(1)).cloned().ok_or("a drawn option")?,
        };
        embedder.table.insert(victim.clone(), degenerate);
        let verdict = SemanticResolver::new(embedder).resolve(scenario.utterance, &scenario.question());
        assert!(
            matches!(
                verdict.resolution(),
                Resolution::Degraded { reason: DegradedReason::UnmeasurableEmbedding }
            ),
            "seed {seed:#x}: {victim:?} degenerate gave {:?}",
            verdict.resolution()
        );
        Ok(())
    })
}

#[test]
/// An utterance aligned with one option resolves to it on the semantic tier,
/// clearing threshold and margin.
fn sim_an_aligned_utterance_resolves_semantically() -> Outcome {
    let mut consulted = 0_usize;
    sweep(|seed, _, scenario| {
        if !scenario.lexicon_absent() {
            return Ok(());
        }
        consulted = consulted.saturating_add(1);
        let resolver = SemanticResolver::new(scenario.embedder(basis(scenario.target))?);
        let policy = resolver.policy();
        match resolver.resolve(scenario.utterance, &scenario.question()).into_resolution() {
            Resolution::Resolved { index, tier, score, lead } => {
                assert_eq!((index, tier), (scenario.target, MatchTier::Semantic), "seed {seed:#x}");
                assert!(score >= policy.threshold() && lead >= policy.margin(), "seed {seed:#x}");
                Ok(())
            }
            other => Err(format!("seed {seed:#x}: aligned gave {other:?}")),
        }
    })?;
    assert!(
        consulted >= DRAWS,
        "the model was consulted on {consulted} draws: the family is close to vacuous"
    );
    Ok(())
}

#[test]
/// An utterance split between two options is `Absent` under the shipped
/// policy and a two-way tie under a looser one.
fn sim_a_split_utterance_is_absent_or_tied_by_policy() -> Outcome {
    sweep(|seed, _, scenario| {
        if !scenario.lexicon_absent() {
            return Ok(());
        }
        let half = std::f32::consts::FRAC_1_SQRT_2;
        let split: Vec<f32> = (0..WIDTH)
            .map(|axis| {
                if axis == scenario.target || axis == scenario.other {
                    half
                } else {
                    0.0
                }
            })
            .collect();
        let shipped = SemanticResolver::new(scenario.embedder(split.clone())?);
        assert!(
            matches!(
                shipped.resolve(scenario.utterance, &scenario.question()).into_resolution(),
                Resolution::Absent { .. }
            ),
            "seed {seed:#x}: 0.707 is under the shipped 0.72"
        );
        let loose = SemanticPolicy::new(0.5, 0.05).map_err(|error| error.to_string())?;
        let looser = SemanticResolver::with_policy(scenario.embedder(split)?, loose);
        match looser.resolve(scenario.utterance, &scenario.question()).into_resolution() {
            Resolution::Ambiguous { tied, .. } => {
                let mut expected = vec![scenario.target, scenario.other];
                expected.sort_unstable();
                assert_eq!(tied, expected, "seed {seed:#x}");
                Ok(())
            }
            other => Err(format!("seed {seed:#x}: a split gave {other:?}")),
        }
    })
}

#[test]
/// A numeric question and a learned phrase are answered without the model.
fn sim_numbers_and_learned_phrases_never_reach_the_model() -> Outcome {
    sweep(|seed, _, scenario| {
        let mut failing = scenario.embedder(basis(0))?;
        failing.table.clear();
        let mut resolver = SemanticResolver::new(failing);
        let numbers = vec!["1".to_owned(), "2".to_owned()];
        let numeric = Question::new("count", &numbers).with_domain(AnswerDomain::Integer);
        let verdict = resolver.resolve("7", &numeric);
        assert!(matches!(verdict.resolution(), Resolution::Absent { .. }), "seed {seed:#x}");
        assert!(verdict.provenance().model().is_none(), "seed {seed:#x}: no model");
        let option = scenario.options.get(scenario.target).ok_or("a drawn option")?;
        resolver.learn("sim", scenario.utterance, option);
        assert_eq!(resolver.learned(), 1, "seed {seed:#x}");
        match resolver.resolve(scenario.utterance, &scenario.question()).into_resolution() {
            Resolution::Resolved { index, tier: MatchTier::Exact, .. } => {
                assert_eq!(index, scenario.target, "seed {seed:#x}");
                Ok(())
            }
            other => Err(format!("seed {seed:#x}: a learned phrase gave {other:?}")),
        }
    })
}

#[test]
/// Equal policies have one version; different policies differ.
fn sim_a_policy_version_is_its_numbers() -> Outcome {
    sweep(|seed, stream, scenario| {
        let draw = |stream: &mut Stream| -> Result<f64, String> {
            stream.choose(&[0.0, 0.05, 0.1, 0.25, 0.5, 0.72, 0.9, 1.0])
        };
        let (first, second) = (
            SemanticPolicy::new(draw(stream)?, draw(stream)?).map_err(|error| error.to_string())?,
            SemanticPolicy::new(draw(stream)?, draw(stream)?).map_err(|error| error.to_string())?,
        );
        let version = |policy: SemanticPolicy| -> Result<String, String> {
            let resolver = SemanticResolver::with_policy(scenario.embedder(basis(0))?, policy);
            Ok(format!("{:?}", resolver.policy_version()))
        };
        assert_eq!(version(first)? == version(second)?, first == second, "seed {seed:#x}");
        Ok(())
    })
}

/// The replay receipt of every verdict drawn under `seed`.
fn verdict_trace(seed: u64) -> Result<u64, String> {
    let mut stream = Stream::new(seed);
    let mut trace = RECEIPT_START;
    for _ in 0..DRAWS {
        let scenario = Scenario::from_stream(&mut stream)?;
        let resolver = SemanticResolver::new(scenario.embedder(basis(scenario.target))?);
        let verdict = resolver.resolve(scenario.utterance, &scenario.question());
        trace = fold(trace, format!("{:?};", verdict.resolution()).as_bytes());
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
