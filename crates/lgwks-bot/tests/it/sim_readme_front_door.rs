//! Simulation family: the README's documented examples under a seeded sweep.
//!
//! `readme_front_door` pins one shape per example: one queue depth, one spec, one registry. This
//! target asks the same questions of *streams and documents*: every order in which a source holds
//! still or moves past a condition, every shape of a spec document, every way a document can be cut
//! short or corrupted, and every number of chains an empty registry has to refuse.
//!
//! | Family | Property it pins |
//! |---|---|
//! | `a_depth_stream_fires_the_chain_exactly_when_the_source_moves_past_the_condition` | a chain fires on the tick its source value moves and the condition holds, and on no other |
//! | `a_drawn_spec_round_trips_through_json_to_the_same_document` | whatever a spec holds, writing it and reading it back writes the same document |
//! | `a_cut_or_corrupted_spec_is_refused_and_never_partly_read` | a strict prefix, an unknown field and a version this build lacks are each refused |
//! | `an_empty_registry_refuses_every_drawn_spec_with_a_need_per_chain` | a registry with no domains admits no document that names one, and says what is missing |
//!
//! # What is real and what is seeded
//!
//! The bot, its chains, the spec parser and the materializer are the shipped ones. The seed decides
//! the stream of depths, the shape of each document and where a document is cut; no clock enters
//! the trace, so a replay that differs is a defect in the code under test.
use std::error::Error;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use lgwks_bot::{Admission, Bot, BotSpec, Cap, DomainRegistry, GrantSet};

use crate::effects::memory_scope;
use crate::readme_front_door::{PageOnCall, QueueDepth};
use crate::sim;

use sim::{Rng, band_of};

/// A test's result: an error fails it with the error's text.
type TestResult = Result<(), Box<dyn Error>>;

/// The conditions a drawn document names, all inside the materializer's closed vocabulary or
/// plainly unknown to it: the parser accepts any identifier, which is exactly what is under test.
const CONDITIONS: [&str; 3] = ["changed", "always", "checks_changed"];

/// One spec document drawn from `rng`, with the name and chain count it was drawn with.
fn drawn_document(rng: &mut Rng) -> Result<(String, String, usize), Box<dyn Error>> {
    let name = format!("bot-{}", rng.below(1000));
    let chains = usize::try_from(rng.between(1, 4))?;
    let mut rendered = Vec::new();
    for chain in 0..chains {
        let mut pairs = Vec::new();
        for pair in 0..rng.between(1, 3) {
            let index = usize::try_from(rng.below(3))?;
            let condition = CONDITIONS
                .get(index)
                .ok_or("a draw below 3 indexes the conditions")?;
            pairs.push(format!(
                r##"["{condition}",{{"domain":"notify::slack","target":"#c{chain}-{pair}"}}]"##
            ));
        }
        rendered.push(format!(
            r#"{{"source":"gh::pr_status","target":"owner/repo-{chain}","on":[{}]}}"#,
            pairs.join(",")
        ));
    }
    let text = format!(
        r#"{{"version":1,"name":"{name}","chains":[{}]}}"#,
        rendered.join(",")
    );
    Ok((name, text, chains))
}

/// A chain fires on the tick its source moves and the condition holds, and on no other.
#[test]
fn a_depth_stream_fires_the_chain_exactly_when_the_source_moves_past_the_condition() -> TestResult {
    sim::assert_replays(band_of(6), |sim| {
        let threshold = sim.rng().between(0, 15);
        let depth = Arc::new(AtomicU32::new(0));
        let pages = Arc::new(AtomicU32::new(0));
        let grants = GrantSet::empty().grant(Cap::net());
        let mut bot = Bot::builder("queue-watch")
            .observe(QueueDepth::over(&depth))
            .on(
                move |reading: &u32| *reading > threshold,
                PageOnCall::counting(&pages),
            )
            .with_effects(memory_scope()?)
            .build(&grants)?;
        let ticks = sim.rng().between(3, 12);
        let mut previous: Option<u32> = None;
        let mut expected_pages = 0_u32;
        for tick in 0..ticks {
            // A source that holds still is as much a case as one that moves.
            let next = match previous {
                Some(held) if sim.rng().below(100) < 40 => held,
                _ => sim.rng().between(0, 20),
            };
            depth.store(next, Ordering::SeqCst);
            let should_fire = previous != Some(next) && next > threshold;
            let fired = bot.tick()?;
            assert!(
                fired <= 1,
                "one chain fires at most one action, got {fired}"
            );
            assert_eq!(
                fired == 1,
                should_fire,
                "tick {tick}: depth {previous:?} -> {next} against the threshold {threshold}"
            );
            expected_pages = expected_pages.saturating_add(u32::from(should_fire));
            previous = Some(next);
            sim.record(&format!("tick {tick} depth={next} fired={fired}"));
        }
        assert_eq!(
            pages.load(Ordering::SeqCst),
            expected_pages,
            "the effect ran once for each tick that fired"
        );
        sim.record(&format!("threshold={threshold} pages={expected_pages}"));
        Ok(())
    })
}

/// Whatever a spec holds, writing it and reading it back writes the same document.
#[test]
fn a_drawn_spec_round_trips_through_json_to_the_same_document() -> TestResult {
    sim::assert_replays(band_of(7), |sim| {
        let (name, text, chains) = drawn_document(sim.rng())?;
        let spec = BotSpec::from_json(&text)?;
        assert_eq!(spec.name(), name, "the name reads back");
        assert_eq!(spec.chains().len(), chains, "every chain reads back");
        let written = spec.to_json()?;
        let again = BotSpec::from_json(&written)?;
        assert_eq!(again.name(), name, "the name survives the round trip");
        assert_eq!(again.chains().len(), chains, "so does every chain");
        assert_eq!(
            again.to_json()?,
            written,
            "writing what was read writes the same document"
        );
        sim.record(&format!(
            "name={name} chains={chains} bytes={}",
            written.len()
        ));
        Ok(())
    })
}

/// A strict prefix, an unknown field and a version this build lacks are each refused.
#[test]
fn a_cut_or_corrupted_spec_is_refused_and_never_partly_read() -> TestResult {
    sim::assert_replays(band_of(8), |sim| {
        let (_name, text, _chains) = drawn_document(sim.rng())?;
        // A strict prefix of a JSON object is never a document.
        let cut = usize::try_from(sim.rng().below(u32::try_from(text.len())?))?;
        let prefix = text.get(..cut).ok_or("the document is ASCII")?;
        assert!(
            BotSpec::from_json(prefix).is_err(),
            "{cut} of {} bytes is not a spec",
            text.len()
        );
        // A field the contract does not name is refused rather than ignored.
        let extra = text.replacen('{', r#"{"unexpected":1,"#, 1);
        assert!(
            BotSpec::from_json(&extra).is_err(),
            "an unknown top-level field must be refused"
        );
        // A version this build does not implement is refused rather than partly interpreted.
        let future = text.replacen(r#""version":1"#, r#""version":2"#, 1);
        assert!(
            BotSpec::from_json(&future).is_err(),
            "a version this build lacks must be refused"
        );
        sim.record(&format!("cut={cut} of {}", text.len()));
        Ok(())
    })
}

/// A registry with no domains admits no document that names one, and says what is missing.
#[test]
fn an_empty_registry_refuses_every_drawn_spec_with_a_need_per_chain() -> TestResult {
    sim::assert_replays(band_of(9), |sim| {
        let (_name, text, chains) = drawn_document(sim.rng())?;
        let spec = BotSpec::from_json(&text)?;
        let refusal = Bot::from_spec(
            &spec,
            &DomainRegistry::empty(),
            &GrantSet::empty(),
            memory_scope()?,
        );
        let Err(Admission::Needs(needs)) = refusal else {
            return Err(format!("expected a NeedSet, got {refusal:?}").into());
        };
        assert!(
            needs.len() >= chains,
            "{chains} chains name {chains} unknown sources, and only {} needs came back",
            needs.len()
        );
        sim.record(&format!("chains={chains} needs={}", needs.len()));
        Ok(())
    })
}
