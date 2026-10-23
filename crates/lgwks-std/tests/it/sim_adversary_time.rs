//! An attacker controls the text handed to `parse_rfc3339`.
//!
//! The attacker is a peer, a log, a config file or a fuzzer: it chooses every
//! byte, including the ones that look like a stamp. The invariants are the ones
//! a caller leans on without checking: the parser never panics, never accepts a
//! byte that is not ASCII, names the exact field, offset and byte it refused, is
//! linear in the input, never lets attacker bytes reach a log line, and reads an
//! offset or a fraction as the instant it names.
//!
//! Sources: RFC 3339 section 5.6 (the grammar), ISO 8601 (calendar bounds),
//! CWE-117 (improper output neutralisation for logs), CWE-20 (improper input
//! validation). Every answer is checked against a reference written here from
//! the grammar, not from the parser.

use std::time::{Duration, SystemTime};

use lgwks_std::time::calendar::try_days_in_month;
use lgwks_std::time::format::to_rfc3339;
use lgwks_std::time::{Field, ParseError, parse_rfc3339};

use crate::adversary::{Attacker, TestResult, as_text, sweep};
use crate::seeded_bytes::fold_bytes;
use crate::seeded_sweep::{
    SWEEP_SEEDS, assert_distinct_seeds_diverge, assert_same_seed_replays, initial_trace,
};

/// A stamp every attack below starts from.
const BASE: &str = "2023-11-14T22:13:20Z";

/// The offset of the byte after the seconds of [`BASE`]: where a fraction or an
/// offset begins.
const AFTER_SECONDS: usize = 19;

/// A drawn civil date and time of day, before any fraction or offset.
#[derive(Clone, Copy)]
struct Civil {
    /// Four-digit year.
    year: u32,
    /// Month, `1..=12`.
    month: u32,
    /// Day of the month, valid for the month and year.
    day: u32,
    /// Hour, `0..=23`.
    hour: u32,
    /// Minute, `0..=59`.
    minute: u32,
    /// Second, `0..=59`.
    second: u32,
}

impl Civil {
    /// The `YYYY-MM-DDTHH:MM:SS` text followed by `tail`.
    fn render(&self, tail: &str) -> String {
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{tail}",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }
}

/// A valid civil date drawn from `years`, a half-open range of four-digit years.
fn draw_civil(attacker: &mut Attacker, years: (u32, u32)) -> Civil {
    let year = attacker.field(years.0, years.1);
    let month = attacker.field(1, 13);
    let Some(length) = try_days_in_month(i64::from(year), month) else {
        // Every month in `1..13` has a length, so this is the first of a month.
        return Civil {
            year,
            month,
            day: 1,
            hour: 0,
            minute: 0,
            second: 0,
        };
    };
    Civil {
        year,
        month,
        day: attacker.field(1, length.saturating_add(1)),
        hour: attacker.field(0, 24),
        minute: attacker.field(0, 60),
        second: attacker.field(0, 60),
    }
}

/// `count` decimal digits drawn from the attacker.
fn digit_run(attacker: &mut Attacker, count: u32) -> String {
    let mut run = String::new();
    for _ in 0..count {
        match char::from_digit(attacker.field(0, 10), 10) {
            Some(digit) => run.push(digit),
            None => run.push('0'),
        }
    }
    run
}

/// A valid zulu stamp from years every platform's clock can name, with a
/// fraction of `narrowest..10` digits (none when the draw is zero).
fn draw_fractional(attacker: &mut Attacker, narrowest: u32) -> String {
    let civil = draw_civil(attacker, (1_700, 9_999));
    let width = attacker.field(narrowest, 10);
    let fraction = if width == 0 {
        String::new()
    } else {
        format!(".{}", digit_run(attacker, width))
    };
    civil.render(&format!("{fraction}Z"))
}

/// Whole seconds in `duration`, as the signed count an offset is measured in.
fn whole_seconds(duration: Duration) -> i64 {
    let Ok(seconds) = i64::try_from(duration.as_secs()) else {
        return i64::MAX;
    };
    seconds
}

/// `left - right` in whole seconds, negative when `left` is earlier.
fn signed_gap(left: SystemTime, right: SystemTime) -> i64 {
    match left.duration_since(right) {
        Ok(gap) => whole_seconds(gap),
        Err(behind) => whole_seconds(behind.duration()).saturating_neg(),
    }
}

/// The offset a refusal reports, when it reports one.
fn offset_of(error: &ParseError) -> Option<usize> {
    match *error {
        ParseError::TooShort { at, .. }
        | ParseError::Malformed { at, .. }
        | ParseError::NonDigit { at, .. }
        | ParseError::OutOfRange { at, .. }
        | ParseError::FractionWidth { at, .. }
        | ParseError::UnsupportedLeapSecond { at }
        | ParseError::MissingOffset { at } => Some(at),
        _ => None,
    }
}

/// One campaign: mutated stamps, every outcome folded into one trace.
fn campaign(seed: u64) -> u64 {
    let mut attacker = Attacker::new(seed);
    let mut trace = initial_trace();
    for _ in 0..256 {
        let civil = draw_civil(&mut attacker, (0, 10_000));
        let stamp = civil.render("Z");
        let hostile = as_text(&attacker.mutate_n(stamp.as_bytes(), 3));
        let outcome = format!("{:?}", parse_rfc3339(&hostile));
        fold_bytes(&mut trace, outcome.as_bytes());
    }
    trace
}

/// The generator draws only stamps the parser accepts, so every refusal below
/// is the attacker's doing and not the fixture's.
#[test]
fn sim_adversary_time_the_generator_draws_only_valid_stamps() -> TestResult {
    sweep(512, |seed, round, attacker| {
        let stamp = draw_civil(attacker, (0, 10_000)).render("Z");
        assert!(
            parse_rfc3339(&stamp).is_ok(),
            "seed {seed:#x} round {round}: the generator drew {stamp:?}, which the parser refused"
        );
        Ok(())
    })
}

/// Three rounds of byte mutation never panic the parser, and whatever it
/// accepts renders to one canonical text that parses back to the same instant.
#[test]
fn sim_adversary_time_mutated_stamps_canonicalise_to_a_fixed_point() -> TestResult {
    sweep(1_024, |seed, round, attacker| {
        let stamp = draw_civil(attacker, (0, 10_000)).render(".25+01:30");
        let hostile = as_text(&attacker.mutate_n(stamp.as_bytes(), 3));
        let Ok(instant) = parse_rfc3339(&hostile) else {
            return Ok(());
        };
        let Ok(canonical) = to_rfc3339(instant) else {
            return Ok(());
        };
        assert_eq!(
            parse_rfc3339(&canonical),
            Ok(instant),
            "seed {seed:#x} round {round}: {hostile:?} rendered as {canonical:?}, which names another instant"
        );
        Ok(())
    })
}

/// A non-digit byte in any digit position is refused naming the field, the
/// offset and the byte, whatever the byte is.
#[test]
fn sim_adversary_time_a_non_digit_in_any_field_is_named_exactly() {
    /// Every digit offset of [`BASE`] and the field it belongs to.
    const POSITIONS: [(usize, Field); 14] = [
        (0, Field::Year),
        (1, Field::Year),
        (2, Field::Year),
        (3, Field::Year),
        (5, Field::Month),
        (6, Field::Month),
        (8, Field::Day),
        (9, Field::Day),
        (11, Field::Hour),
        (12, Field::Hour),
        (14, Field::Minute),
        (15, Field::Minute),
        (17, Field::Second),
        (18, Field::Second),
    ];
    for (at, field) in POSITIONS {
        for &junk in b"x -:/+.zZT\x7f\x00\x1f" {
            let mut bytes = BASE.as_bytes().to_vec();
            bytes[at] = junk;
            assert_eq!(
                parse_rfc3339(&as_text(&bytes)),
                Err(ParseError::NonDigit {
                    field,
                    at,
                    byte: junk
                }),
                "byte {junk:#04x} at offset {at} must be refused as a non-digit in {field}"
            );
        }
    }
}

/// A glyph that is not ASCII, inserted anywhere, makes the stamp unparseable:
/// the grammar has no non-ASCII production, so no position can absorb one.
#[test]
fn sim_adversary_time_a_non_ascii_glyph_anywhere_is_refused() {
    for glyph in ["\u{e9}", "\u{663}", "\u{65e5}", "\u{1f600}", "\u{ff12}"] {
        for at in 0..=BASE.len() {
            let mut text = String::from(BASE);
            text.insert_str(at, glyph);
            assert!(
                parse_rfc3339(&text).is_err(),
                "{glyph:?} inserted at offset {at} was accepted: {text:?}"
            );
        }
    }
}

/// A Unicode decimal digit standing where an ASCII digit goes is not a digit
/// here: Arabic-Indic and fullwidth digits are the classic lookalikes.
#[test]
fn sim_adversary_time_lookalike_digits_are_not_digits() {
    for glyph in ["\u{663}", "\u{ff12}", "\u{9e8}", "\u{1d7d8}"] {
        for at in [0_usize, 3, 5, 6, 8, 9, 11, 12, 14, 15, 17, 18] {
            let mut text = String::from(BASE);
            text.replace_range(at..at.saturating_add(1), glyph);
            assert!(
                parse_rfc3339(&text).is_err(),
                "{glyph:?} replacing the digit at offset {at} was accepted: {text:?}"
            );
        }
    }
}

/// A fraction outside one to nine digits is refused with the exact width it
/// had, however long, and a width inside the range is accepted. The refusal
/// costs one pass: a mebibyte of digits is a bounded answer, not a stall.
#[test]
fn sim_adversary_time_a_fraction_bomb_is_refused_with_its_width() {
    for digits in [0_usize, 10, 11, 64, 4_096, 1_048_576] {
        let text = format!("2023-11-14T22:13:20.{}Z", "7".repeat(digits));
        assert_eq!(
            parse_rfc3339(&text),
            Err(ParseError::FractionWidth {
                digits,
                at: AFTER_SECONDS
            }),
            "a {digits}-digit fraction must be refused with its width"
        );
    }
    for digits in 1_usize..=9 {
        let text = format!("2023-11-14T22:13:20.{}Z", "7".repeat(digits));
        assert!(
            parse_rfc3339(&text).is_ok(),
            "a {digits}-digit fraction is inside the profile and must parse"
        );
    }
}

/// Any ASCII byte after the zulu designator is refused at the byte after it:
/// a stamp is not a prefix, so a smuggled tail never parses.
#[test]
fn sim_adversary_time_a_tail_after_zulu_is_refused_at_its_first_byte() {
    for byte in 0_u8..128 {
        for zulu in ['Z', 'z'] {
            let mut text = String::from(&BASE[..AFTER_SECONDS]);
            text.push(zulu);
            text.push(char::from(byte));
            assert_eq!(
                parse_rfc3339(&text),
                Err(ParseError::Malformed { at: 20, byte }),
                "byte {byte:#04x} after {zulu:?} must be refused at offset 20"
            );
        }
    }
}

/// Any ASCII byte after a numeric offset is refused at the byte after it.
#[test]
fn sim_adversary_time_a_tail_after_a_numeric_offset_is_refused() {
    for byte in 0_u8..128 {
        let mut text = String::from("2023-11-14T22:13:20+02:00");
        text.push(char::from(byte));
        assert_eq!(
            parse_rfc3339(&text),
            Err(ParseError::Malformed { at: 25, byte }),
            "byte {byte:#04x} after the offset must be refused at offset 25"
        );
    }
}

/// A numeric offset cut short at any width is a missing offset, located where
/// the offset began.
#[test]
fn sim_adversary_time_a_truncated_offset_is_a_missing_offset() {
    for sign in ['+', '-'] {
        let full = format!("{sign}02:00");
        for keep in 1..full.len() {
            let text = format!("{}{}", &BASE[..AFTER_SECONDS], &full[..keep]);
            assert_eq!(
                parse_rfc3339(&text),
                Err(ParseError::MissingOffset { at: AFTER_SECONDS }),
                "{text:?} is an offset cut after {keep} bytes"
            );
        }
    }
}

/// Control bytes and spaces in front of a stamp are refused at offset zero:
/// the parser does not trim, so padding never changes what is accepted.
#[test]
fn sim_adversary_time_leading_padding_is_refused_at_offset_zero() {
    for byte in (0_u8..=0x20).chain([0x7f]) {
        let text = format!("{}{BASE}", char::from(byte));
        assert_eq!(
            parse_rfc3339(&text),
            Err(ParseError::NonDigit {
                field: Field::Year,
                at: 0,
                byte
            }),
            "byte {byte:#04x} in front of a stamp must be refused at offset 0"
        );
    }
}

/// Every proper prefix shorter than the minimum stamp is refused with the
/// length it had, and the stamp without its offset is a missing offset.
#[test]
fn sim_adversary_time_a_short_prefix_names_its_length() {
    for len in 0..AFTER_SECONDS {
        assert_eq!(
            parse_rfc3339(&BASE[..len]),
            Err(ParseError::TooShort { len, at: len }),
            "a {len}-byte prefix is below the minimum stamp length"
        );
    }
    assert_eq!(
        parse_rfc3339(&BASE[..AFTER_SECONDS]),
        Err(ParseError::MissingOffset { at: AFTER_SECONDS }),
        "the stamp without its offset is a missing offset"
    );
}

/// Every separator position refuses any other byte, and the date-time
/// separator accepts exactly `T`, `t` and a space.
#[test]
fn sim_adversary_time_a_wrong_separator_is_named() {
    for at in [4_usize, 7, 13, 16] {
        for &junk in b"x/_.: +" {
            if junk == BASE.as_bytes()[at] {
                continue;
            }
            let mut bytes = BASE.as_bytes().to_vec();
            bytes[at] = junk;
            assert_eq!(
                parse_rfc3339(&as_text(&bytes)),
                Err(ParseError::Malformed { at, byte: junk }),
                "byte {junk:#04x} at separator offset {at} must be refused"
            );
        }
    }
    for &junk in b"x/_.:-+Zz" {
        let mut bytes = BASE.as_bytes().to_vec();
        bytes[10] = junk;
        assert_eq!(
            parse_rfc3339(&as_text(&bytes)),
            Err(ParseError::Malformed { at: 10, byte: junk }),
            "byte {junk:#04x} as the date-time separator must be refused"
        );
    }
}

/// The accepted spellings of the separator and of the zulu designator name one
/// instant: an alias is a spelling, never a second meaning.
#[test]
fn sim_adversary_time_accepted_aliases_name_one_instant() {
    let canonical = parse_rfc3339(BASE);
    assert!(canonical.is_ok(), "the base stamp must parse");
    for text in [
        "2023-11-14t22:13:20Z",
        "2023-11-14 22:13:20Z",
        "2023-11-14T22:13:20z",
        "2023-11-14t22:13:20z",
        "2023-11-14 22:13:20z",
        "2023-11-14T22:13:20+00:00",
        "2023-11-14T22:13:20-00:00",
    ] {
        assert_eq!(
            parse_rfc3339(text),
            canonical,
            "{text:?} must name the base instant"
        );
    }
}

/// Two stamps run together are one malformed stamp, refused where the first
/// ends: not two stamps, and not the first.
#[test]
fn sim_adversary_time_concatenated_stamps_are_refused_at_the_seam() {
    for (joiner, first) in [("", b'2'), (",", b','), (" ", b' '), (";", b';')] {
        let joined = format!("{BASE}{joiner}{BASE}");
        assert_eq!(
            parse_rfc3339(&joined),
            Err(ParseError::Malformed {
                at: 20,
                byte: first
            }),
            "{joined:?} must be refused at the seam"
        );
    }
}

/// A leap second is refused for every date, with or without a fraction and an
/// offset, and seconds past the leap value are out of range at the field.
#[test]
fn sim_adversary_time_a_leap_second_is_refused_whatever_surrounds_it() -> TestResult {
    sweep(128, |seed, _round, attacker| {
        let mut civil = draw_civil(attacker, (0, 10_000));
        civil.second = 60;
        for tail in ["Z", ".999999999Z", "+23:59", "-00:30", ".5-12:00"] {
            assert_eq!(
                parse_rfc3339(&civil.render(tail)),
                Err(ParseError::UnsupportedLeapSecond { at: 17 }),
                "seed {seed:#x}: second 60 with tail {tail:?} must be a leap-second refusal"
            );
        }
        civil.second = attacker.field(61, 100);
        assert_eq!(
            parse_rfc3339(&civil.render("Z")),
            Err(ParseError::OutOfRange {
                field: Field::Second,
                value: civil.second,
                min: 0,
                max: 59,
                at: 17
            }),
            "seed {seed:#x}: second {} must be out of range",
            civil.second
        );
        Ok(())
    })
}

/// Each field one past its bound is refused naming the field, the value, the
/// inclusive bounds and the offset the field began at.
#[test]
fn sim_adversary_time_every_field_one_past_its_bound_is_named() {
    let cases = [
        ("2023-00-14T22:13:20Z", Field::Month, 0, 1, 12, 5),
        ("2023-13-14T22:13:20Z", Field::Month, 13, 1, 12, 5),
        ("2023-11-00T22:13:20Z", Field::Day, 0, 1, 30, 8),
        ("2023-11-31T22:13:20Z", Field::Day, 31, 1, 30, 8),
        ("2023-12-32T22:13:20Z", Field::Day, 32, 1, 31, 8),
        ("2023-11-14T24:13:20Z", Field::Hour, 24, 0, 23, 11),
        ("2023-11-14T99:13:20Z", Field::Hour, 99, 0, 23, 11),
        ("2023-11-14T22:60:20Z", Field::Minute, 60, 0, 59, 14),
        ("2023-11-14T22:99:20Z", Field::Minute, 99, 0, 59, 14),
        (
            "2023-11-14T22:13:20+24:00",
            Field::OffsetHour,
            24,
            0,
            23,
            20,
        ),
        (
            "2023-11-14T22:13:20-00:60",
            Field::OffsetMinute,
            60,
            0,
            59,
            23,
        ),
    ];
    for (text, field, value, min, max, at) in cases {
        assert_eq!(
            parse_rfc3339(text),
            Err(ParseError::OutOfRange {
                field,
                value,
                min,
                max,
                at
            }),
            "{text:?} must be refused as {field} {value}"
        );
    }
}

/// A numeric offset is subtracted exactly: the same civil time written with
/// `Z` is later than the same civil time written `+hh:mm` by that offset, and
/// earlier than it when written `-hh:mm`.
#[test]
fn sim_adversary_time_an_offset_moves_the_instant_by_exactly_its_width() -> TestResult {
    sweep(256, |seed, round, attacker| {
        let civil = draw_civil(attacker, (100, 9_900));
        let (hours, minutes) = (attacker.field(0, 24), attacker.field(0, 60));
        let width = i64::from(hours)
            .saturating_mul(3_600)
            .saturating_add(i64::from(minutes).saturating_mul(60));
        let zulu = parse_rfc3339(&civil.render("Z"))?;
        for (sign, expected) in [('+', width), ('-', width.saturating_neg())] {
            let tail = format!("{sign}{hours:02}:{minutes:02}");
            let shifted = parse_rfc3339(&civil.render(&tail))?;
            assert_eq!(
                signed_gap(zulu, shifted),
                expected,
                "seed {seed:#x} round {round}: offset {tail} moved the instant by the wrong width"
            );
        }
        Ok(())
    })
}

/// A fraction is read left-aligned to nanoseconds: `.5` is half a second and
/// `.000000001` is one nanosecond, for every width and every digit string.
#[test]
fn sim_adversary_time_a_fraction_is_the_nanoseconds_it_spells() -> TestResult {
    sweep(256, |seed, round, attacker| {
        let civil = draw_civil(attacker, (100, 9_900));
        let width = attacker.field(1, 10);
        let run = digit_run(attacker, width);
        let nanos = format!("{run:0<9}").parse::<u64>()?;
        let whole = parse_rfc3339(&civil.render("Z"))?;
        let split = parse_rfc3339(&civil.render(&format!(".{run}Z")))?;
        assert_eq!(
            split.duration_since(whole)?,
            Duration::from_nanos(nanos),
            "seed {seed:#x} round {round}: fraction .{run} must be {nanos} ns"
        );
        Ok(())
    })
}

/// Every offset a refusal reports lies inside the input, so a caller that
/// slices the text at the reported offset cannot be sent past its end.
#[test]
fn sim_adversary_time_a_reported_offset_never_leaves_the_input() -> TestResult {
    sweep(1_024, |seed, round, attacker| {
        let stamp = draw_civil(attacker, (0, 10_000)).render(".5+01:00");
        let hostile = as_text(&attacker.mutate_n(stamp.as_bytes(), 4));
        let Err(error) = parse_rfc3339(&hostile) else {
            return Ok(());
        };
        let Some(at) = offset_of(&error) else {
            return Ok(());
        };
        assert!(
            at <= hostile.len(),
            "seed {seed:#x} round {round}: {error} reports offset {at} in {} bytes",
            hostile.len()
        );
        Ok(())
    })
}

/// A refusal's message never carries a line break or an escape, so hostile
/// bytes cannot forge a log line through it (CWE-117).
#[test]
fn sim_adversary_time_a_refusal_message_cannot_forge_a_log_line() -> TestResult {
    sweep(1_024, |seed, round, attacker| {
        let stamp = draw_civil(attacker, (0, 10_000)).render("Z");
        let hostile = as_text(&attacker.mutate_n(stamp.as_bytes(), 4));
        let Err(error) = parse_rfc3339(&hostile) else {
            return Ok(());
        };
        let message = error.to_string();
        assert!(
            !message.contains(['\n', '\r', '\u{1b}', '\0']),
            "seed {seed:#x} round {round}: {message:?} carries a control byte from {hostile:?}"
        );
        assert!(!message.is_empty(), "a refusal must say something");
        Ok(())
    })
}

/// An accepted stamp renders in one canonical shape: `T` and `Z`, a fraction of
/// one to nine digits with no trailing zero, nothing else.
#[test]
fn sim_adversary_time_accepted_input_renders_in_one_canonical_shape() -> TestResult {
    sweep(512, |seed, round, attacker| {
        let stamp = draw_fractional(attacker, 0);
        let text = to_rfc3339(parse_rfc3339(&stamp)?)?;
        let bytes = text.as_bytes();
        let last = bytes.len().saturating_sub(1);
        assert!(
            text.is_ascii(),
            "seed {seed:#x} round {round}: {text:?} not ASCII"
        );
        assert!(
            bytes.len() == 20 || (22..=30).contains(&bytes.len()),
            "seed {seed:#x} round {round}: {text:?} has a length no canonical stamp has"
        );
        assert_eq!(bytes[10], b'T', "seed {seed:#x}: {text:?} separator");
        assert_eq!(bytes[last], b'Z', "seed {seed:#x}: {text:?} zulu");
        if bytes.len() > 20 {
            assert_ne!(
                bytes[last.saturating_sub(1)],
                b'0',
                "seed {seed:#x}: {text:?} has a trailing zero in its fraction"
            );
        }
        Ok(())
    })
}

/// An instant survives render and parse to the nanosecond, for every instant an
/// attacker can reach by choosing the stamp.
#[test]
fn sim_adversary_time_a_chosen_instant_survives_a_render_parse_cycle() -> TestResult {
    sweep(512, |seed, round, attacker| {
        let first = parse_rfc3339(&draw_fractional(attacker, 1))?;
        let text = to_rfc3339(first)?;
        assert_eq!(
            parse_rfc3339(&text)?,
            first,
            "seed {seed:#x} round {round}: {text:?} drifted"
        );
        Ok(())
    })
}

/// The same seed mounts the same attack and gets the same outcomes.
#[test]
fn sim_adversary_time_the_same_seed_replays_the_same_campaign() {
    for seed in SWEEP_SEEDS {
        assert_same_seed_replays(campaign, seed);
    }
}

/// Two seeds mount two attacks: the campaign reads its seed.
#[test]
fn sim_adversary_time_distinct_seeds_mount_distinct_campaigns() {
    assert_distinct_seeds_diverge(campaign, SWEEP_SEEDS[0], SWEEP_SEEDS[1]);
    assert_distinct_seeds_diverge(campaign, SWEEP_SEEDS[2], SWEEP_SEEDS[3]);
}
