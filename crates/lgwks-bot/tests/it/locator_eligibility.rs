//! Public regression tests for locator ownership and near-miss evidence.

use lgwks_bot::interface::{Anchor, ElementFacts, Ladder, Recognition, RecognitionVector};

fn control(
    tag: &str,
    frame: usize,
    anchor: Option<(&str, Anchor)>,
    identity: &str,
) -> ElementFacts {
    let facts = ElementFacts::new(
        tag,
        "html > body > form > button",
        "Submit",
        [0.5, 0.5, 0.1, 0.05],
    )
    .with_frames(vec![frame])
    .with_identity(vec![String::from(identity)]);
    anchor.map_or(facts.clone(), |(value, kind)| {
        facts.with_anchor(kind, value)
    })
}

fn vector() -> Result<RecognitionVector, Box<dyn std::error::Error>> {
    Ok(RecognitionVector::fingerprint()?)
}

fn ladder(kind: Anchor) -> Result<Ladder, Box<dyn std::error::Error>> {
    Ok(Ladder::new([kind])?)
}

#[test]
fn flat_and_ladder_resolve_the_same_eligible_control() -> Result<(), Box<dyn std::error::Error>> {
    let target = control("button", 0, Some(("submit", Anchor::Id)), "shared");
    let candidates = [target.clone()];
    let recognizer = vector()?;

    assert_eq!(
        recognizer.recognize(&target, &candidates),
        Recognition::Resolved {
            index: 0,
            score: 1.0,
            lead: 1.0,
        }
    );
    assert_eq!(
        recognizer.recognize_with_ladder(&ladder(Anchor::Id)?, &target, &candidates),
        Recognition::Resolved {
            index: 0,
            score: 1.0,
            lead: 1.0,
        }
    );
    Ok(())
}

#[test]
fn both_recognizers_reject_wrong_frames_and_incompatible_kinds()
-> Result<(), Box<dyn std::error::Error>> {
    let target = control("button", 0, Some(("submit", Anchor::Id)), "shared");
    let wrong_frame = control("button", 1, Some(("submit", Anchor::Id)), "shared");
    let wrong_kind = control("img", 0, Some(("submit", Anchor::Id)), "shared");
    let recognizer = vector()?;
    let id_ladder = ladder(Anchor::Id)?;

    for candidate in [wrong_frame, wrong_kind] {
        assert_eq!(
            recognizer.recognize(&target, std::slice::from_ref(&candidate)),
            Recognition::Absent { best_score: 0.0 }
        );
        assert_eq!(
            recognizer.recognize_with_ladder(&id_ladder, &target, &[candidate]),
            Recognition::Absent { best_score: 0.0 }
        );
    }
    Ok(())
}

#[test]
fn ladder_ignores_cross_frame_duplicates_but_refuses_eligible_duplicates()
-> Result<(), Box<dyn std::error::Error>> {
    let target = control("button", 0, Some(("submit", Anchor::Id)), "shared");
    let cross_frame = control("button", 1, Some(("submit", Anchor::Id)), "shared");
    let same_frame = control("button", 0, Some(("submit", Anchor::Id)), "shared");
    let recognizer = vector()?;
    let id_ladder = ladder(Anchor::Id)?;

    assert_eq!(
        recognizer.recognize_with_ladder(&id_ladder, &target, &[cross_frame, target.clone()]),
        Recognition::Resolved {
            index: 1,
            score: 1.0,
            lead: 1.0,
        }
    );
    assert_eq!(
        recognizer.recognize_with_ladder(&id_ladder, &target, &[same_frame, target.clone()]),
        Recognition::Ambiguous {
            best: 0,
            runner_up: Some(1),
            score: 1.0,
            lead: 0.0,
        }
    );
    Ok(())
}

#[test]
fn absent_anchor_is_not_resolved_and_near_miss_keeps_its_score()
-> Result<(), Box<dyn std::error::Error>> {
    let target = control("button", 0, Some(("old", Anchor::Id)), "shared");
    let absent = control("button", 0, Some(("cancel", Anchor::Id)), "different");
    let high_score_near_miss = control("button", 0, Some(("new", Anchor::Id)), "shared");
    let recognizer = vector()?;
    let id_ladder = ladder(Anchor::Id)?;

    assert_eq!(
        recognizer.recognize_with_ladder(&id_ladder, &target, &[absent]),
        Recognition::Absent { best_score: 0.5 }
    );
    assert_eq!(
        recognizer.recognize(&target, std::slice::from_ref(&high_score_near_miss)),
        Recognition::Resolved {
            index: 0,
            score: 1.0,
            lead: 1.0,
        }
    );
    assert_eq!(
        recognizer.recognize_with_ladder(&id_ladder, &target, &[high_score_near_miss]),
        Recognition::Absent { best_score: 1.0 }
    );
    Ok(())
}
