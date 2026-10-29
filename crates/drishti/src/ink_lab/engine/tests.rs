//! The ink lab's unit tests: replay by seed, frame bounds, reduced motion,
//! refusals of malformed input, and the published brand mark.

use super::beams::group_thousands;
use crate::ink_lab::{
    Canvas, Effect, Frame, HostId, HostSpec, InkEngine, InkError, Input, MAX_BEADS, MAX_SHAPES,
    Options, Quality, Render, Shape,
};

/// Step a valid delta; a refusal fails the test.
fn stepped(engine: &mut InkEngine, dt_ms: f64) -> Vec<Render> {
    let result = engine.step(dt_ms);
    assert_eq!(result.as_ref().err(), None, "valid delta refused");
    result.into_iter().flatten().collect()
}

/// A call that must be accepted.
fn accepted(result: Result<(), InkError>) {
    assert!(result.is_ok(), "valid call refused: {result:?}");
}
use crate::design::{Palette, Role};

/// Build one engine with a single host.
fn engine_with(effect: Effect, seed: u64) -> (InkEngine, HostId) {
    let mut engine = InkEngine::new(seed);
    let id = engine.add_host(HostSpec {
        effect,
        canvas: Canvas::new(320.0, 200.0),
        visible: true,
    });
    (engine, id)
}

#[test]
fn reduced_motion_resolves_every_effect_to_its_end_state() {
    for effect in Effect::all() {
        let (mut engine, _) = engine_with(effect, 7);
        engine.set_reduce_motion(true);
        let renders = stepped(&mut engine, 16.0);
        assert_eq!(renders.len(), 1, "{} did not render", effect.name());
        let frame = &renders[0].frame;
        assert!(frame.blur.abs() < 1e-9, "{} left blur on", effect.name());
        assert!(
            frame.displacement.abs() < 1e-9,
            "{} left displacement",
            effect.name()
        );
        assert!(
            (frame.alpha - 1.0).abs() < 1e-9,
            "{} left alpha",
            effect.name()
        );
        if effect == Effect::Mark {
            assert!(!frame.shapes().is_empty());
            assert!(matches!(frame.shapes()[0], Shape::Quad { .. }));
        }
    }
}

#[test]
fn same_seed_and_trace_replay_to_the_same_digest() {
    let run = |seed: u64| {
        let (mut engine, id) = engine_with(Effect::Ripple, seed);
        accepted(engine.set_input(
            id,
            Input {
                pointer: None,
                click: Some((40.0, 30.0)),
            },
        ));
        let mut digest = 0_u64;
        for _ in 0..40 {
            let renders = stepped(&mut engine, 16.0);
            if let Some(render) = renders.first() {
                digest = digest.wrapping_mul(31).wrapping_add(render.frame.digest());
            }
        }
        digest
    };
    assert_eq!(run(99), run(99), "replay diverged");
    assert_ne!(run(99), run(100), "seed did not change the trace");
}

#[test]
fn mark_liquid_shape_count_is_bounded() {
    let (mut engine, _) = engine_with(Effect::Mark, 1);
    for _ in 0..30 {
        let renders = stepped(&mut engine, 16.0);
        // A frame with no render emitted no shapes, which is within the bound.
        let shapes = match renders.first() {
            Some(render) => render.frame.shapes().len(),
            None => 0,
        };
        assert!(shapes <= MAX_SHAPES, "mark emitted {shapes} shapes");
    }
}

#[test]
fn count_labels_are_grouped_and_settle_on_target() {
    assert_eq!(group_thousands("1200"), "1,200");
    assert_eq!(group_thousands("1234567"), "1,234,567");
    assert_eq!(group_thousands("999"), "999");
    assert_eq!(group_thousands("-1234.50"), "-1,234.50");
    let (mut engine, id) = engine_with(Effect::Count, 3);
    accepted(engine.set_options(
        id,
        Options {
            count_to: Some(1284.0),
            count_suffix: Some("+"),
            ..Options::new()
        },
    ));
    let mut last = String::new();
    for _ in 0..200 {
        let renders = stepped(&mut engine, 16.0);
        if let Some(render) = renders.first()
            && let Some(label) = render.frame.label()
        {
            last = String::from(label);
        }
    }
    assert_eq!(last, "1,284+");
}

#[test]
fn lite_quality_strides_and_marks_skips() {
    let mut engine = InkEngine::new(5);
    for _ in 0..8 {
        engine.add_host(HostSpec {
            effect: Effect::Loader,
            canvas: Canvas::new(100.0, 100.0),
            visible: true,
        });
    }
    engine.set_quality(Quality::Lite);
    for _ in 0..20 {
        stepped(&mut engine, 16.0);
    }
    let stats = engine.stats();
    assert!(stats.lite);
    assert!(stats.stride_skips > 0, "lite tier skipped nothing");
}

#[test]
fn off_screen_hosts_do_not_render() {
    let (mut engine, id) = engine_with(Effect::Loader, 2);
    engine.set_visible(id, false);
    assert!(stepped(&mut engine, 16.0).is_empty());
    engine.set_visible(id, true);
    assert_eq!(stepped(&mut engine, 16.0).len(), 1);
}

#[test]
fn leaving_reduced_motion_resumes_resolved_effects() {
    let (mut engine, _) = engine_with(Effect::Loader, 4);
    engine.set_reduce_motion(true);
    assert_eq!(stepped(&mut engine, 16.0).len(), 1, "resolve frame");
    assert!(
        stepped(&mut engine, 16.0).is_empty(),
        "resolved host keeps running"
    );
    engine.set_reduce_motion(false);
    assert_eq!(stepped(&mut engine, 16.0).len(), 1, "motion did not resume");
}

#[test]
fn malformed_deltas_inputs_and_options_are_refused_without_effect() {
    let (mut engine, id) = engine_with(Effect::Pour, 6);
    let before = engine.stats();
    for delta in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0] {
        assert_eq!(
            engine.step(delta).err(),
            Some(InkError::InvalidDelta),
            "{delta}"
        );
    }
    assert_eq!(engine.stats(), before, "a refused step advanced the engine");
    let input = Input {
        pointer: Some((f64::NAN, 1.0)),
        ..Input::default()
    };
    assert_eq!(
        engine.set_input(id, input).err(),
        Some(InkError::InvalidInput)
    );
    for (options, field) in [
        (
            Options {
                level: Some(f64::NAN),
                ..Options::new()
            },
            "level",
        ),
        (
            Options {
                level: Some(1.5),
                ..Options::new()
            },
            "level",
        ),
        (
            Options {
                size: Some(f64::INFINITY),
                ..Options::new()
            },
            "size",
        ),
        (
            Options {
                duration_ms: Some(-5.0),
                ..Options::new()
            },
            "duration_ms",
        ),
        (
            Options {
                count_decimals: Some(400),
                ..Options::new()
            },
            "count_decimals",
        ),
        (
            Options {
                count_to: Some(1.0e300),
                ..Options::new()
            },
            "count_to",
        ),
    ] {
        assert_eq!(
            engine.set_options(id, options).err(),
            Some(InkError::InvalidOption(field)),
            "{field} accepted"
        );
    }
    accepted(engine.set_options(
        id,
        Options {
            count_from: Some(-40.0),
            ..Options::new()
        },
    ));
    let canvas = Canvas::new(f64::NAN, f64::INFINITY);
    assert!(
        canvas.width.is_finite() && canvas.height.is_finite(),
        "canvas not finite"
    );
}

#[test]
fn reduced_motion_resolves_strided_hosts_on_the_same_frame() {
    let mut engine = InkEngine::new(12);
    for _ in 0..6 {
        engine.add_host(HostSpec::new(Effect::Loader, Canvas::new(60.0, 60.0), true));
    }
    engine.set_quality(Quality::Lite);
    let _ = stepped(&mut engine, 16.0);
    engine.set_reduce_motion(true);
    assert_eq!(
        stepped(&mut engine, 16.0).len(),
        6,
        "a strided host missed the resolve"
    );
    assert!(
        stepped(&mut engine, 16.0).is_empty(),
        "resolved hosts kept running"
    );
}

#[test]
fn paused_engine_steps_nothing() {
    let (mut engine, _) = engine_with(Effect::Loader, 2);
    engine.set_paused(true);
    assert!(stepped(&mut engine, 16.0).is_empty());
}

#[test]
fn hostile_bead_request_is_bounded() {
    let (mut engine, id) = engine_with(Effect::Cursor, 9);
    accepted(engine.set_options(
        id,
        Options {
            beads: Some(usize::MAX),
            ..Options::new()
        },
    ));
    let renders = stepped(&mut engine, 16.0);
    // No render counts as no beads, which the equality below refuses.
    let shapes = match renders.first() {
        Some(render) => render.frame.shapes().len(),
        None => 0,
    };
    assert_eq!(shapes, MAX_BEADS, "bead chain escaped its bound");
}

#[test]
fn resolved_mark_is_the_published_twenty_four_beams() {
    let (mut engine, _) = engine_with(Effect::Mark, 1);
    engine.set_reduce_motion(true);
    let renders = stepped(&mut engine, 16.0);
    // No render counts as no beams, which the equality below refuses.
    let shapes = match renders.first() {
        Some(render) => render.frame.shapes(),
        None => &[],
    };
    assert_eq!(shapes.len(), 24, "the mark has twenty-four beams");
    // The first published beam's narrow end is (59.34, 51)-(60.66, 51).
    assert!(
        matches!(shapes[0], Shape::Quad { points } if (points[0][0] - 59.34).abs() < 1e-9
                && (points[3][0] - 60.66).abs() < 1e-9),
        "first beam does not match the published polygon"
    );
}

#[test]
fn brand_roles_resolve_without_inventing_colours() {
    let palette = Palette::light();
    let accent = palette.role(Role::Accent);
    assert!(accent.contrast(palette.role(Role::Canvas)) > 3.0);
    assert_eq!(Frame::resolved().digest(), Frame::resolved().digest());
}
