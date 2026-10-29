//! Seeded simulations of the pieces the session harness drives only
//! indirectly: the brand colour arithmetic, the percentile reader, the option
//! and canvas domains, and the ink engine called directly.
//!
//! Each test draws its inputs from one [`Rng`] seed per case and checks the
//! library against an independent model written here, so a failure names the
//! seed that replays it. Sources: WCAG 2.2 §1.4.3 and §1.4.6 with the
//! relative-luminance definition (contrast is `(L1 + 0.05) / (L2 + 0.05)`),
//! the nearest-rank percentile definition, and the option domains and replay
//! rule of `docs/ink-engine.md`.

use std::collections::BTreeSet;

use drishti::design::{Colourway, Rgb, Wcag};
use drishti::ink_lab::{
    Canvas, Effect, HostId, HostSpec, InkEngine, InkError, Input, MAX_CANVAS, MAX_COUNT_DECIMALS,
    MAX_OPTION_AMOUNT, MAX_OPTION_MS, MAX_OPTION_PIXELS, MAX_SHAPES, Options, Quality,
};
use drishti::runtime::Percentiles;

use crate::harness::{Rng, TRACE_SEED, mix};

/// Cases each simulation draws.
const CASES: u64 = 512;

/// One 60 Hz frame.
const FRAME: f64 = 1000.0 / 60.0;

/// A colour drawn from `rng`.
fn colour(rng: &mut Rng) -> Rgb {
    let [red, green, blue, _, _, _, _, _] = rng.next_u64().to_be_bytes();
    Rgb::new(red, green, blue)
}

/// The WCAG 2.2 relative luminance, computed here rather than by the library.
fn luminance_model(rgb: Rgb) -> f64 {
    let linear = |channel: u8| {
        let value = f64::from(channel) / 255.0;
        if value <= 0.039_28 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(rgb.red) + 0.7152 * linear(rgb.green) + 0.0722 * linear(rgb.blue)
}

/// A grade's rank, strictest last.
const fn rank(grade: Wcag) -> u8 {
    match grade {
        Wcag::Aaa => 3,
        Wcag::Aa => 2,
        Wcag::LargeOnly => 1,
        _ => 0,
    }
}

/// Run `case` once per seed of `base..base + CASES`.
fn sweep(base: u64, mut case: impl FnMut(u64, &mut Rng)) {
    for seed in base..base.saturating_add(CASES) {
        let mut rng = Rng::new(seed);
        case(seed, &mut rng);
    }
}

#[test]
fn sim_a_colour_round_trips_through_its_hex_spelling() {
    sweep(100_000, |seed, rng| {
        let rgb = colour(rng);
        let hex = rgb.hex();
        assert_eq!(hex.len(), 7, "seed {seed}: {hex}");
        assert!(hex.starts_with('#'), "seed {seed}: {hex}");
        assert_eq!(hex, hex.to_ascii_lowercase(), "seed {seed}: not lowercase");
        assert_eq!(Rgb::from_hex(&hex), Some(rgb), "seed {seed}: {hex}");
    });
}

#[test]
fn sim_any_corrupted_hex_digit_is_refused() {
    sweep(101_000, |seed, rng| {
        let hex = colour(rng).hex();
        let (Some(at), Some(bad)) = (
            rng.pick(&[1_usize, 2, 3, 4, 5, 6]),
            rng.pick(&['g', 'z', ' ', '#', '-', 'G']),
        ) else {
            return;
        };
        let corrupted: String = hex
            .chars()
            .enumerate()
            .map(|(index, ch)| if index == at { bad } else { ch })
            .collect();
        assert_eq!(Rgb::from_hex(&corrupted), None, "seed {seed}: {corrupted}");
        let short: String = hex.chars().take(at).collect();
        assert_eq!(Rgb::from_hex(&short), None, "seed {seed}: {short}");
    });
}

#[test]
fn sim_upper_case_and_bare_spellings_name_the_same_colour() {
    sweep(102_000, |seed, rng| {
        let rgb = colour(rng);
        let hex = rgb.hex();
        let bare = hex.trim_start_matches('#').to_owned();
        assert_eq!(
            Rgb::from_hex(&hex.to_ascii_uppercase()),
            Some(rgb),
            "seed {seed}"
        );
        assert_eq!(Rgb::from_hex(&bare), Some(rgb), "seed {seed}");
        assert_eq!(
            Rgb::from_hex(&bare.to_ascii_uppercase()),
            Some(rgb),
            "seed {seed}"
        );
    });
}

#[test]
fn sim_luminance_is_the_wcag_formula_and_rises_with_every_channel() {
    sweep(103_000, |seed, rng| {
        let rgb = colour(rng);
        let measured = rgb.luminance();
        assert!((0.0..=1.0).contains(&measured), "seed {seed}: {measured}");
        assert!(
            (measured - luminance_model(rgb)).abs() < 1e-12,
            "seed {seed}: library and model disagree"
        );
        let brighter = [
            Rgb::new(rgb.red.saturating_add(1), rgb.green, rgb.blue),
            Rgb::new(rgb.red, rgb.green.saturating_add(1), rgb.blue),
            Rgb::new(rgb.red, rgb.green, rgb.blue.saturating_add(1)),
        ];
        for lighter in brighter {
            assert!(lighter.luminance() >= measured, "seed {seed}: {lighter:?}");
        }
    });
}

#[test]
fn sim_contrast_is_symmetric_bounded_and_one_against_itself() {
    sweep(104_000, |seed, rng| {
        let (first, second) = (colour(rng), colour(rng));
        let ratio = first.contrast(second);
        assert!((1.0..=21.0).contains(&ratio), "seed {seed}: {ratio}");
        assert!(
            (ratio - second.contrast(first)).abs() < 1e-12,
            "seed {seed}"
        );
        assert!((first.contrast(first) - 1.0).abs() < 1e-12, "seed {seed}");
        let (one, two) = (luminance_model(first), luminance_model(second));
        let (high, low) = if one >= two { (one, two) } else { (two, one) };
        assert!(
            (ratio - (high + 0.05) / (low + 0.05)).abs() < 1e-9,
            "seed {seed}: ratio is not (L1 + 0.05) / (L2 + 0.05)"
        );
    });
}

#[test]
fn sim_every_grade_agrees_with_the_threshold_table() {
    sweep(105_000, |seed, rng| {
        let ratio = rng.range(1.0, 21.0);
        let expected = |large: bool| {
            if ratio >= 7.0 {
                Wcag::Aaa
            } else if ratio >= 4.5 {
                Wcag::Aa
            } else if large && ratio >= 3.0 {
                Wcag::LargeOnly
            } else {
                Wcag::Fail
            }
        };
        assert_eq!(
            Wcag::grade(ratio, false),
            expected(false),
            "seed {seed}: {ratio}"
        );
        assert_eq!(
            Wcag::grade(ratio, true),
            expected(true),
            "seed {seed}: {ratio}"
        );
    });
}

#[test]
fn sim_large_text_is_never_graded_more_strictly() {
    sweep(106_000, |seed, rng| {
        let (first, second) = (colour(rng), colour(rng));
        let ratio = first.contrast(second);
        assert!(
            rank(Wcag::grade(ratio, true)) >= rank(Wcag::grade(ratio, false)),
            "seed {seed}: large text graded below body text at {ratio}"
        );
    });
}

#[test]
fn sim_a_colourway_reads_as_its_measured_luminances_say() {
    let ways = Colourway::all();
    sweep(107_000, |seed, rng| {
        let Some(way) = rng.pick(&ways) else { return };
        let paint = way.paint();
        let ground = luminance_model(paint.ground);
        for (name, ink) in [("ink", paint.ink), ("accent", paint.accent)] {
            let fore = luminance_model(ink);
            let (high, low) = if fore >= ground {
                (fore, ground)
            } else {
                (ground, fore)
            };
            let model = (high + 0.05) / (low + 0.05);
            assert!(
                (ink.contrast(paint.ground) - model).abs() < 1e-9,
                "seed {seed}: {} {name}",
                way.id()
            );
        }
    });
}

/// The nearest-rank value at `permille`, from a sorted copy.
fn nearest_rank(sorted: &[f64], permille: usize) -> Option<f64> {
    let index = sorted
        .len()
        .saturating_mul(permille)
        .div_ceil(1000)
        .saturating_sub(1);
    sorted.get(index).copied()
}

/// Between one and a thousand seeded samples.
fn samples(rng: &mut Rng) -> Vec<f64> {
    let count = rng.below(1000).saturating_add(1);
    (0..count).map(|_| rng.range(0.0, 50.0)).collect()
}

#[test]
fn sim_percentiles_are_the_nearest_rank_of_the_sorted_samples() {
    sweep(108_000, |seed, rng| {
        let drawn = samples(rng);
        let mut sorted = drawn.clone();
        sorted.sort_by(f64::total_cmp);
        let stats = Percentiles::of(&drawn);
        assert_eq!(stats.samples, drawn.len(), "seed {seed}");
        assert_eq!(Some(stats.p50), nearest_rank(&sorted, 500), "seed {seed}");
        assert_eq!(Some(stats.p95), nearest_rank(&sorted, 950), "seed {seed}");
        assert_eq!(Some(stats.p99), nearest_rank(&sorted, 990), "seed {seed}");
        assert_eq!(Some(stats.max), sorted.last().copied(), "seed {seed}");
    });
}

#[test]
fn sim_percentiles_do_not_depend_on_sample_order() {
    sweep(109_000, |seed, rng| {
        let drawn = samples(rng);
        let mut shuffled = drawn.clone();
        // Fisher-Yates from the same seed's stream.
        for top in (1..shuffled.len()).rev() {
            let Ok(bound) = u64::try_from(top.saturating_add(1)) else {
                continue;
            };
            if let Ok(pick) = usize::try_from(rng.below(bound)) {
                shuffled.swap(top, pick);
            }
        }
        assert_eq!(
            Percentiles::of(&drawn),
            Percentiles::of(&shuffled),
            "seed {seed}"
        );
    });
}

#[test]
fn sim_percentiles_are_ordered_and_an_empty_set_says_so() {
    sweep(110_000, |seed, rng| {
        let stats = Percentiles::of(&samples(rng));
        assert!(stats.p50 <= stats.p95, "seed {seed}");
        assert!(stats.p95 <= stats.p99, "seed {seed}");
        assert!(stats.p99 <= stats.max, "seed {seed}");
    });
    let empty = Percentiles::of(&[]);
    assert_eq!(empty.samples, 0, "an empty set measured something");
}

#[test]
fn sim_a_canvas_is_clamped_whatever_it_is_given() {
    sweep(111_000, |seed, rng| {
        let axis = |rng: &mut Rng| match rng.below(6) {
            0 => f64::NAN,
            1 => f64::INFINITY,
            2 => f64::NEG_INFINITY,
            3 => -rng.range(0.0, 1.0e9),
            4 => rng.range(MAX_CANVAS, 1.0e12),
            _ => rng.range(0.0, MAX_CANVAS),
        };
        let (width, height) = (axis(rng), axis(rng));
        let canvas = Canvas::new(width, height);
        for (given, kept) in [(width, canvas.width), (height, canvas.height)] {
            assert!(kept.is_finite(), "seed {seed}: {given} kept as {kept}");
            assert!(
                (4.0..=MAX_CANVAS).contains(&kept),
                "seed {seed}: {given} -> {kept}"
            );
            if given.is_finite() && (4.0..=MAX_CANVAS).contains(&given) {
                assert!(
                    (kept - given).abs() < f64::EPSILON,
                    "seed {seed}: moved {given}"
                );
            }
        }
    });
}

/// Set option field `field` to `value`, returning the name validation reports.
fn set_field(options: &mut Options, field: u64, value: f64) -> &'static str {
    match field {
        0 => {
            options.duration_ms = Some(value);
            "duration_ms"
        }
        1 => {
            options.hold_ms = Some(value);
            "hold_ms"
        }
        2 => {
            options.stagger_ms = Some(value);
            "stagger_ms"
        }
        3 => {
            options.auto_ms = Some(value);
            "auto_ms"
        }
        4 => {
            options.blur = Some(value);
            "blur"
        }
        5 => {
            options.displacement = Some(value);
            "displacement"
        }
        6 => {
            options.size = Some(value);
            "size"
        }
        7 => {
            options.level = Some(value);
            "level"
        }
        8 => {
            options.spout = Some(value);
            "spout"
        }
        9 => {
            options.amount = Some(value);
            "amount"
        }
        _ => {
            options.hover = Some(value);
            "hover"
        }
    }
}

/// The upper bound of option field `field`'s domain.
const fn ceiling(field: u64) -> f64 {
    match field {
        0..=3 => MAX_OPTION_MS,
        4..=6 => MAX_OPTION_PIXELS,
        7 | 8 => 1.0,
        _ => MAX_OPTION_AMOUNT,
    }
}

#[test]
fn sim_one_option_out_of_its_domain_is_refused_by_name() {
    sweep(112_000, |seed, rng| {
        let field = rng.below(11);
        let high = ceiling(field);
        let value = match rng.below(4) {
            0 => f64::NAN,
            1 => f64::INFINITY,
            2 => -rng.range(f64::EPSILON, 1.0e6),
            _ => rng.range(high * 1.001 + 1.0e-9, high * 10.0 + 1.0),
        };
        let mut options = Options::new();
        let name = set_field(&mut options, field, value);
        assert_eq!(
            options.validate(),
            Err(InkError::InvalidOption(name)),
            "seed {seed}: {name} = {value}"
        );
    });
}

#[test]
fn sim_options_inside_every_domain_are_accepted() {
    sweep(113_000, |seed, rng| {
        let mut options = Options::new();
        for field in 0..11 {
            if rng.below(2) == 0 {
                let _named = set_field(&mut options, field, rng.range(0.0, ceiling(field)));
            }
        }
        let decimals = usize::from(rng.below_u16(13));
        options.count_decimals = Some(decimals.min(MAX_COUNT_DECIMALS));
        options.count_to = Some(rng.range(-1.0e12, 1.0e12));
        options.beads = Some(usize::MAX);
        assert_eq!(options.validate(), Ok(()), "seed {seed}: {options:?}");
    });
}

/// The digest of every render of `steps` frames, in order.
fn digests(engine: &mut InkEngine, steps: u64) -> Vec<u64> {
    let mut out = Vec::new();
    for _ in 0..steps {
        if let Ok(renders) = engine.step(FRAME) {
            out.extend(renders.iter().map(|render| render.frame.digest()));
        }
    }
    out
}

/// An engine from `seed` with one host per effect in `effects`.
fn engine(seed: u64, effects: &[Effect]) -> (InkEngine, Vec<HostId>) {
    let mut engine = InkEngine::new(seed);
    let hosts = effects
        .iter()
        .map(|&effect| engine.add_host(HostSpec::new(effect, Canvas::new(248.0, 168.0), true)))
        .collect();
    (engine, hosts)
}

#[test]
fn sim_a_refused_delta_leaves_the_engine_where_it_was() {
    sweep(114_000, |seed, rng| {
        let Some(effect) = rng.pick(&Effect::all()) else {
            return;
        };
        let Some(bad) = rng.pick(&[f64::NAN, f64::INFINITY, -1.0, f64::NEG_INFINITY]) else {
            return;
        };
        let (mut struck, _) = engine(seed, &[effect]);
        let (mut clean, _) = engine(seed, &[effect]);
        let lead = rng.below(30);
        assert_eq!(
            digests(&mut struck, lead),
            digests(&mut clean, lead),
            "seed {seed}"
        );
        let before = struck.stats().frames;
        assert_eq!(
            struck.step(bad).err(),
            Some(InkError::InvalidDelta),
            "seed {seed}"
        );
        assert_eq!(
            struck.stats().frames,
            before,
            "seed {seed}: a refused step counted"
        );
        assert_eq!(
            digests(&mut struck, 40),
            digests(&mut clean, 40),
            "seed {seed}"
        );
    });
}

#[test]
fn sim_refused_input_keeps_the_input_before_it() {
    sweep(115_000, |seed, rng| {
        let Some(effect) = rng.pick(&[Effect::Wobble, Effect::Cursor, Effect::Ripple]) else {
            return;
        };
        let (mut struck, struck_hosts) = engine(seed, &[effect]);
        let (mut clean, clean_hosts) = engine(seed, &[effect]);
        let mut good = Input::default();
        good.pointer = Some((rng.range(0.0, 248.0), rng.range(0.0, 168.0)));
        good.click = good.pointer;
        let mut bad = good;
        bad.click = Some((f64::NAN, rng.range(0.0, 168.0)));
        for (target, hosts) in [(&mut struck, &struck_hosts), (&mut clean, &clean_hosts)] {
            for &host in hosts {
                assert_eq!(target.set_input(host, good), Ok(()), "seed {seed}");
            }
        }
        for &host in &struck_hosts {
            assert_eq!(
                struck.set_input(host, bad),
                Err(InkError::InvalidInput),
                "seed {seed}"
            );
        }
        assert_eq!(
            digests(&mut struck, 60),
            digests(&mut clean, 60),
            "seed {seed}"
        );
    });
}

#[test]
fn sim_a_hidden_host_never_renders() {
    sweep(116_000, |seed, rng| {
        let effects = Effect::all();
        let (mut ink, hosts) = engine(seed, &effects);
        let hidden: BTreeSet<u32> = hosts
            .iter()
            .filter(|_| rng.below(2) == 0)
            .map(|host| host.0)
            .collect();
        for &host in &hosts {
            ink.set_visible(host, !hidden.contains(&host.0));
        }
        for _ in 0..rng.below(90).saturating_add(1) {
            let Ok(renders) = ink.step(rng.range(4.0, 40.0)) else {
                continue;
            };
            for render in &renders {
                assert!(
                    !hidden.contains(&render.host.0),
                    "seed {seed}: {:?}",
                    render.host
                );
                assert!(render.frame.shapes().len() <= MAX_SHAPES, "seed {seed}");
            }
        }
    });
}

#[test]
fn sim_reduced_motion_resolves_the_engine_then_it_sleeps() {
    sweep(117_000, |seed, rng| {
        let Some(effect) = rng.pick(&Effect::all()) else {
            return;
        };
        let (mut ink, _) = engine(seed, &[effect]);
        let _warm = digests(&mut ink, rng.below(120));
        ink.set_reduce_motion(true);
        let resolved = ink.step(FRAME);
        assert!(
            resolved.is_ok(),
            "seed {seed}: the resolving step was refused"
        );
        if let Ok(renders) = resolved {
            for render in &renders {
                let frame = &render.frame;
                assert!(
                    frame.blur.abs() < 1e-12,
                    "seed {seed}: {} blurred",
                    effect.name()
                );
                assert!((frame.alpha - 1.0).abs() < 1e-12, "seed {seed}: faded");
            }
        }
        let after = ink.step(FRAME);
        assert!(
            matches!(after, Ok(ref renders) if renders.is_empty()),
            "seed {seed}: {} kept rendering after it resolved",
            effect.name()
        );
    });
}

/// `value` with a comma between each group of three digits.
fn grouped(value: u64) -> String {
    let digits = value.to_string();
    let lead = digits.len() % 3;
    let mut out = String::with_capacity(digits.len().saturating_mul(2));
    for (index, ch) in digits.chars().enumerate() {
        if index != 0 && index % 3 == lead {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

#[test]
fn sim_a_count_settles_on_its_grouped_target() {
    sweep(118_000, |seed, rng| {
        let Ok(target) = u32::try_from(rng.below(1_000_000_000)) else {
            return;
        };
        let (mut ink, hosts) = engine(seed, &[Effect::Count]);
        let mut options = Options::new();
        options.count_from = Some(0.0);
        options.count_to = Some(f64::from(target));
        options.count_decimals = Some(0);
        for &host in &hosts {
            assert_eq!(ink.set_options(host, options), Ok(()), "seed {seed}");
        }
        ink.set_reduce_motion(true);
        let mut label = None;
        if let Ok(renders) = ink.step(FRAME) {
            for render in renders {
                label = render.frame.label().map(str::to_owned);
            }
        }
        let expected = grouped(u64::from(target));
        assert_eq!(
            label.as_deref(),
            Some(expected.as_str()),
            "seed {seed}: {target}"
        );
    });
}

#[test]
fn sim_one_seed_and_one_schedule_replay_the_same_frames() {
    sweep(119_000, |seed, rng| {
        let effects = Effect::all();
        let (mut first, first_hosts) = engine(seed, &effects);
        let (mut second, second_hosts) = engine(seed, &effects);
        let mut trace = (mix(TRACE_SEED, seed), mix(TRACE_SEED, seed));
        for _ in 0..rng.below(80).saturating_add(20) {
            let roll = rng.below(10);
            let slot = usize::from(rng.below_u16(11));
            let quality = rng.pick(&[Quality::Auto, Quality::Lite, Quality::Full]);
            let speed = rng.below_u32(3000);
            let dt = rng.range(0.0, 60.0);
            for (ink, hosts, hash) in [
                (&mut first, &first_hosts, &mut trace.0),
                (&mut second, &second_hosts, &mut trace.1),
            ] {
                match (roll, hosts.get(slot), quality) {
                    (0, Some(&host), _) => ink.set_visible(host, false),
                    (1, Some(&host), _) => ink.set_visible(host, true),
                    (2, _, Some(quality)) => ink.set_quality(quality),
                    (3, _, _) => ink.set_speed_milli(speed),
                    _ => {
                        if let Ok(renders) = ink.step(dt) {
                            for render in renders {
                                *hash = mix(
                                    mix(*hash, u64::from(render.host.0)),
                                    render.frame.digest(),
                                );
                            }
                        }
                    }
                }
            }
        }
        assert_eq!(trace.0, trace.1, "seed {seed}: one schedule, two traces");
    });
}

#[test]
fn sim_the_frame_counter_counts_exactly_the_accepted_steps() {
    sweep(120_000, |seed, rng| {
        let Some(effect) = rng.pick(&Effect::all()) else {
            return;
        };
        let (mut ink, _) = engine(seed, &[effect]);
        let mut accepted = 0_u64;
        for _ in 0..rng.below(200) {
            let dt = match rng.below(5) {
                0 => f64::NAN,
                1 => -rng.range(f64::EPSILON, 100.0),
                _ => rng.range(0.0, 50.0),
            };
            if ink.step(dt).is_ok() {
                accepted = accepted.saturating_add(1);
            }
        }
        assert_eq!(ink.stats().frames, accepted, "seed {seed}");
    });
}
