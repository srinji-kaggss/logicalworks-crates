//! Simulation family: the session module's front doors under a seeded sweep.
//!
//! `session_front_door` pins one shape per door: one ask, one answer, one tightened axis, one
//! template. This target asks the same questions of *streams*: every order in which unrecognized
//! and recognized answers arrive, every axis and every request size against the byte ceilings, every
//! template length, and many sessions over one flow at once.
//!
//! | Family | Property it pins |
//! |---|---|
//! | `an_answer_stream_ends_the_flow_at_the_first_recognized_answer` | unrecognized answers leave the ask current; the first recognized one completes the flow |
//! | `an_answer_after_the_end_is_refused_however_many_follow` | every answer past the end is `SessionTerminated` and charges nothing |
//! | `sessions_over_one_flow_never_share_a_transcript` | interleaved answers to several sessions change only the session they were given to |
//! | `a_tightened_axis_reads_back_and_is_refused_only_above_its_ceiling` | any request reads back exactly, moves no other axis, and is refused iff it exceeds the ceiling |
//! | `a_template_has_one_part_per_literal_and_marker` | a drawn template splits into exactly its literals and markers and cannot be sized without values |
//! | `a_phrase_resolves_to_the_same_option_every_time` | the resolver gives a known option its exact tier and an unrelated phrase `Absent`, on every draw |
//!
//! # What is real and what is seeded
//!
//! The session, its flow, the resolver, the limits and the template compiler are the shipped ones.
//! The seed decides only *which* answers arrive, in what order, and how large a request is; no clock
//! and no scheduler enters the trace, so a replay that differs is a defect in the code under test.
use std::collections::BTreeMap;
use std::error::Error;

use lgwks_bot::language::LanguageResolver;
use lgwks_bot::session::{MatchTier, Question, Resolution, Session, Terminal};
use lgwks_bot::{BotError, CompiledTemplate, Resolver, ResourceAxis, ResourceLimits, VarScope};

use crate::session_front_door::size_flow;
use crate::sim;

use sim::{Rng, band_of};

/// A test's result: an error fails it with the error's text.
type TestResult = Result<(), Box<dyn Error>>;

/// What a flow's ask recognizes, and what it does not.
///
/// The two unrecognized phrases are the ones the resolver is known to leave `Absent`, so the
/// property under test is about *order*, not about a fuzzy matcher's edge.
const ANSWERS: [&str; 4] = ["small", "large", "something else entirely", "no idea"];

/// Whether `answer` names one of the ask's options.
fn recognized(answer: &str) -> bool {
    answer == "small" || answer == "large"
}

/// One answer drawn from [`ANSWERS`].
fn draw_answer(rng: &mut Rng) -> Result<&'static str, Box<dyn Error>> {
    let index = usize::try_from(rng.below(4))?;
    ANSWERS
        .get(index)
        .copied()
        .ok_or_else(|| "a draw below 4 indexes the four answers".into())
}

/// Unrecognized answers leave the ask current; the first recognized one completes the flow.
#[test]
fn an_answer_stream_ends_the_flow_at_the_first_recognized_answer() -> TestResult {
    sim::assert_replays(band_of(0), |sim| {
        let mut session = Session::new("demo", size_flow()?)?;
        let length = sim.rng().between(1, 8);
        let mut ended_at = None;
        for index in 0..length {
            let answer = draw_answer(sim.rng())?;
            let outcome = session.answer(answer);
            if ended_at.is_some() {
                assert!(
                    matches!(outcome, Err(BotError::SessionTerminated)),
                    "answer {index} came after the end and must be refused"
                );
                continue;
            }
            outcome?;
            if recognized(answer) {
                ended_at = Some(index);
            } else {
                assert_eq!(
                    session.current(),
                    Some("ask_size"),
                    "an unrecognized answer leaves the ask current"
                );
            }
            sim.record(&format!("answer {index} {answer:?}"));
        }
        assert_eq!(
            session.terminal() == Some(&Terminal::Completed),
            ended_at.is_some(),
            "the flow completed exactly when a recognized answer arrived"
        );
        sim.record(&format!("length={length} ended_at={ended_at:?}"));
        Ok(())
    })
}

/// Every answer past the end is `SessionTerminated`, and none of them charges a step.
#[test]
fn an_answer_after_the_end_is_refused_however_many_follow() -> TestResult {
    sim::assert_replays(band_of(1), |sim| {
        let mut session = Session::new("demo", size_flow()?)?;
        session.answer(if sim.rng().below(2) == 0 {
            "small"
        } else {
            "large"
        })?;
        let steps = session.steps();
        let transcript = session.transcript().len();
        let extra = sim.rng().between(1, 12);
        for _ in 0..extra {
            let answer = draw_answer(sim.rng())?;
            assert!(
                matches!(session.answer(answer), Err(BotError::SessionTerminated)),
                "an answer after the end is refused, whatever it says"
            );
        }
        assert_eq!(session.steps(), steps, "a refused answer charges no step");
        assert_eq!(
            session.transcript().len(),
            transcript,
            "a refused answer writes nothing to the transcript"
        );
        sim.record(&format!("extra={extra} steps={steps}"));
        Ok(())
    })
}

/// Interleaved answers to several sessions change only the session they were given to.
#[test]
fn sessions_over_one_flow_never_share_a_transcript() -> TestResult {
    sim::assert_replays(band_of(2), |sim| {
        let tenants = usize::try_from(sim.rng().between(2, 5))?;
        let mut sessions = Vec::new();
        for tenant in 0..tenants {
            sessions.push(Session::new(
                format!("tenant-{tenant}").as_str(),
                size_flow()?,
            )?);
        }
        let mut done = vec![false; tenants];
        for _ in 0..sim.rng().between(4, 16) {
            let which = usize::try_from(sim.rng().below(u32::try_from(tenants)?))?;
            let answer = draw_answer(sim.rng())?;
            let (Some(session), Some(finished)) = (sessions.get_mut(which), done.get_mut(which))
            else {
                return Err("a drawn tenant indexes the sessions".into());
            };
            let outcome = session.answer(answer);
            if *finished {
                assert!(
                    matches!(outcome, Err(BotError::SessionTerminated)),
                    "tenant {which} already ended"
                );
            } else {
                outcome?;
                *finished = recognized(answer);
            }
            sim.record(&format!("tenant {which} {answer:?} finished={finished}"));
        }
        for (tenant, (session, finished)) in sessions.iter().zip(&done).enumerate() {
            assert_eq!(
                session.terminal() == Some(&Terminal::Completed),
                *finished,
                "tenant {tenant} ended by its own answers and no other's"
            );
            if !*finished {
                assert_eq!(
                    session.current(),
                    Some("ask_size"),
                    "tenant {tenant} is still on the ask"
                );
            }
        }
        Ok(())
    })
}

/// Any request reads back exactly, moves no other axis, and is refused iff it exceeds the ceiling.
#[test]
fn a_tightened_axis_reads_back_and_is_refused_only_above_its_ceiling() -> TestResult {
    const AXES: [ResourceAxis; 4] = [
        ResourceAxis::Utterance,
        ResourceAxis::Value,
        ResourceAxis::Record,
        ResourceAxis::Session,
    ];
    sim::assert_replays(band_of(3), |sim| {
        let shipped = ResourceLimits::shipped();
        let index = usize::try_from(sim.rng().below(4))?;
        let axis = *AXES.get(index).ok_or("a draw below 4 indexes the axes")?;
        let ceiling = shipped.get(axis);
        // Up to twice the ceiling, so both sides of it are drawn; a draw is a `u32`, which holds
        // every shipped ceiling.
        let limit = u32::try_from(ceiling.saturating_mul(2))?;
        let requested = usize::try_from(sim.rng().between(1, limit))?;
        let tight = shipped.tighten(axis, requested);
        assert_eq!(tight.get(axis), requested, "the request reads back");
        for other in AXES.iter().filter(|other| **other != axis) {
            assert_eq!(
                tight.get(*other),
                shipped.get(*other),
                "tightening {axis:?} moved {other:?}"
            );
        }
        match tight.within_ceiling() {
            Ok(_) => assert!(requested <= ceiling, "{requested} is above {ceiling}"),
            Err(BotError::ResourceLimitAboveCeiling {
                axis: named,
                requested: asked,
                ceiling: allowed,
                ..
            }) => {
                assert!(requested > ceiling, "{requested} is inside {ceiling}");
                assert_eq!(named, axis, "the refusal names the raised axis");
                assert_eq!(asked, requested, "the refusal names the request");
                assert_eq!(allowed, ceiling, "the refusal names the ceiling");
            }
            Err(other) => return Err(other.into()),
        }
        // The stricter of two sets wins axis by axis, whichever way round they are given.
        let narrow = shipped.tighten(axis, 1);
        assert_eq!(
            tight.narrowed(narrow),
            narrow.narrowed(tight),
            "narrowing is symmetric"
        );
        sim.record(&format!("axis={axis:?} requested={requested}"));
        Ok(())
    })
}

/// A drawn template splits into exactly its literals and markers and cannot be sized without values.
#[test]
fn a_template_has_one_part_per_literal_and_marker() -> TestResult {
    sim::assert_replays(band_of(4), |sim| {
        let segments = sim.rng().between(1, 5);
        let mut text = String::new();
        let mut parts = 0_usize;
        for index in 0..segments {
            let word = sim.rng().below(1000).to_string();
            let marker = index.to_string();
            text.push_str(&["word", &word, "-${v", &marker, "}"].concat());
            parts = parts.saturating_add(2);
        }
        if sim.rng().below(2) == 0 {
            text.push('!');
            parts = parts.saturating_add(1);
        }
        let compiled = CompiledTemplate::compile(&text)?;
        assert_eq!(
            compiled.parts().len(),
            parts,
            "{text:?} has {parts} parts: its literals and its markers"
        );
        let scope = VarScope::new(BTreeMap::new())?;
        assert!(
            compiled.expanded_bytes(&scope).is_err(),
            "{text:?} cannot be sized while its names have no value"
        );
        sim.record(&format!("segments={segments} parts={parts}"));
        Ok(())
    })
}

/// The resolver gives a known option its exact tier and an unrelated phrase `Absent`, every time.
#[test]
fn a_phrase_resolves_to_the_same_option_every_time() -> TestResult {
    sim::assert_replays(band_of(5), |sim| {
        let resolver = LanguageResolver::new();
        let options = ["small", "large"].map(String::from).to_vec();
        let question = Question::new("ask_size", &options);
        for _ in 0..sim.rng().between(1, 6) {
            let phrase = draw_answer(sim.rng())?;
            let resolution = resolver.resolve(phrase, &question).into_resolution();
            match phrase {
                "small" | "large" => {
                    let expected = usize::from(phrase == "large");
                    assert!(
                        matches!(
                            resolution,
                            Resolution::Resolved { index, tier: MatchTier::Exact, .. }
                                if index == expected
                        ),
                        "{phrase:?} resolves exactly to option {expected}, got {resolution:?}"
                    );
                }
                _ => assert!(
                    matches!(resolution, Resolution::Absent { .. }),
                    "{phrase:?} names no option, got {resolution:?}"
                ),
            }
            sim.record(&format!("phrase {phrase:?}"));
        }
        Ok(())
    })
}
