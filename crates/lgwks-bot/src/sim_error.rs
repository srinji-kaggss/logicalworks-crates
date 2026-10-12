//! Seeded simulation of the error vocabulary's two promises: untrusted text
//! cannot forge a log record, and a retry decision reads the variant, never
//! the message.
//!
//! One seed draws hostile text — newlines, carriage returns, tabs, ESC, NUL,
//! DEL, C1 controls, multi-byte letters — and drawn failures, and every family
//! asserts what a log reader or a retry policy sees:
//!
//! - escaped text holds no control character, so it is one physical line that
//!   cannot reach a terminal;
//! - escaping is idempotent, leaves text with no control character unchanged,
//!   and decodes back to the original exactly;
//! - a denial renders its count, then every capability and the domain that
//!   required it, each escaped, on one line;
//! - every document-sourced field of a rendered error is escaped;
//! - each dispatch certainty maps to one retry class, and a domain error's
//!   class is its certainty's whatever its cause says;
//! - the same seed replays the same renderings and distinct seeds diverge.
//!
//! Sources: CWE-117 (improper output neutralization for logs), CVE-2021-44228's
//! lesson that log text is an attack surface, and ECMA-48 for the escape
//! sequences a terminal executes.

use std::fmt::Write as _;

use crate::cap::{Cap, Demand};
use crate::error::{BotError, DispatchCertainty, Escaped, RetryClass};
use crate::gate::GrantSet;
use crate::sim_stream::{RECEIPT_START, Stream, fold};

/// The seeds every family replays.
const SEEDS: [u64; 4] = [
    0xE5C4_0000_0000_0001,
    0xE5C4_0000_0000_0002,
    0xE5C4_FFFF_0000_0003,
    0x10C5_E5C4_0000_0004,
];

/// Texts each family draws per seed.
const DRAWS: usize = 256;

/// The glyphs hostile text is drawn from: controls a log line or a terminal
/// acts on, and plain and multi-byte letters that must survive untouched.
const GLYPHS: [char; 16] = [
    'a', 'Z', '7', ' ', '.', ':', 'é', '✓', '𝄞', '\n', '\r', '\t', '\u{1b}', '\u{0}', '\u{7f}',
    '\u{85}',
];

/// A family's result.
type Outcome = Result<(), String>;

/// Up to 48 drawn glyphs.
fn hostile(stream: &mut Stream) -> Result<String, String> {
    let len = stream.below(49)?;
    (0..len).map(|_| stream.choose(&GLYPHS)).collect()
}

/// Every drawn text under each seed.
fn sweep(mut check: impl FnMut(u64, &mut Stream, &str) -> Outcome) -> Outcome {
    for seed in SEEDS {
        let mut stream = Stream::new(seed);
        for _ in 0..DRAWS {
            let text = hostile(&mut stream)?;
            check(seed, &mut stream, &text)?;
        }
    }
    Ok(())
}

/// The inverse of the escaping: `\n`, `\r`, `\t` and `\u{hex}` back to their
/// characters, for text that held no backslash before it was escaped.
fn unescape(escaped: &str) -> Result<String, String> {
    let mut out = String::with_capacity(escaped.len());
    let mut glyphs = escaped.chars();
    while let Some(glyph) = glyphs.next() {
        if glyph != '\\' {
            out.push(glyph);
            continue;
        }
        match glyphs.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('u') => {
                if glyphs.next() != Some('{') {
                    return Err(format!("a \\u escape without its brace in {escaped:?}"));
                }
                let hex: String = glyphs.by_ref().take_while(|digit| *digit != '}').collect();
                let code = u32::from_str_radix(&hex, 16).map_err(|error| error.to_string())?;
                out.push(char::from_u32(code).ok_or_else(|| format!("{code:x} is no char"))?);
            }
            other => return Err(format!("an unknown escape {other:?} in {escaped:?}")),
        }
    }
    Ok(out)
}

#[test]
/// Escaped text holds no control character.
fn sim_escaped_text_holds_no_control_character() -> Outcome {
    sweep(|seed, _, text| {
        let escaped = Escaped(text).to_string();
        assert!(
            !escaped.chars().any(char::is_control),
            "seed {seed:#x}: {text:?} escaped to {escaped:?}"
        );
        Ok(())
    })
}

#[test]
/// Escaping twice is escaping once.
fn sim_escaping_is_idempotent() -> Outcome {
    sweep(|seed, _, text| {
        let once = Escaped(text).to_string();
        assert_eq!(
            Escaped(&once).to_string(),
            once,
            "seed {seed:#x}: {text:?}"
        );
        Ok(())
    })
}

#[test]
/// Text with no control character is written exactly as given.
fn sim_plain_text_is_unchanged() -> Outcome {
    sweep(|seed, _, text| {
        let plain: String = text.chars().filter(|glyph| !glyph.is_control()).collect();
        assert_eq!(Escaped(&plain).to_string(), plain, "seed {seed:#x}");
        Ok(())
    })
}

#[test]
/// The escaped text decodes back to the original exactly: nothing was lost.
fn sim_escaped_text_decodes_to_the_original() -> Outcome {
    sweep(|seed, _, text| {
        let escaped = Escaped(text).to_string();
        assert_eq!(
            unescape(&escaped)?,
            text,
            "seed {seed:#x}: {escaped:?} lost something"
        );
        Ok(())
    })
}

#[test]
/// A denial renders its count, then every capability and the domain that
/// required it, each escaped, on one line.
fn sim_a_denial_renders_every_shortage_on_one_line() -> Outcome {
    sweep(|seed, stream, text| {
        let count = stream.below(4)?.saturating_add(1);
        let required: Vec<Cap> = (0..count)
            .map(|index| Cap::new(format!("{text}#{index}")))
            .collect();
        let domain = format!("dom{text}");
        let shortages = GrantSet::empty().uncovered(&required, &Demand::new(domain.as_str()));
        let deficit = crate::cap::Deficit::from_shortages(shortages)
            .ok_or_else(|| format!("seed {seed:#x}: nothing was short"))?;
        let mut expected = format!("{count} ungranted");
        for (index, cap) in required.iter().enumerate() {
            let separator = if index == 0 { ": " } else { ", " };
            write!(
                expected,
                "{separator}{} (required by {})",
                Escaped(cap.as_str()),
                Escaped(&domain)
            )
            .map_err(|error| error.to_string())?;
        }
        let rendered = deficit.to_string();
        assert_eq!(rendered, expected, "seed {seed:#x}");
        assert!(
            !rendered.chars().any(char::is_control),
            "seed {seed:#x}: one physical line"
        );
        Ok(())
    })
}

#[test]
/// Every document-sourced field of a rendered error is escaped.
fn sim_document_sourced_fields_render_escaped() -> Outcome {
    sweep(|seed, _, text| {
        let errors = [
            BotError::UnregisteredDomain {
                domain: text.to_owned(),
            },
            BotError::UnjournaledEffect {
                domain: text.to_owned(),
            },
            BotError::IncompleteSpec {
                field: "name",
                cause: text.to_owned(),
            },
        ];
        for error in errors {
            let rendered = error.to_string();
            assert!(
                !rendered.chars().any(char::is_control),
                "seed {seed:#x}: {rendered:?}"
            );
            assert!(
                rendered.contains(&Escaped(text).to_string()),
                "seed {seed:#x}: the field is rendered whole: {rendered:?}"
            );
        }
        Ok(())
    })
}

/// The retry class each certainty permits, as the vocabulary states it.
fn class_of(certainty: DispatchCertainty) -> RetryClass {
    match certainty {
        DispatchCertainty::Refused | DispatchCertainty::Occurred => RetryClass::Never,
        DispatchCertainty::NotDelivered => RetryClass::Safe,
        DispatchCertainty::Unsettled => RetryClass::RequiresEvidence,
    }
}

#[test]
/// A domain error's class is its certainty's, whatever its cause says.
fn sim_a_domain_errors_class_is_its_certaintys() -> Outcome {
    sweep(|seed, stream, text| {
        let certainty = stream.choose(&[
            DispatchCertainty::Refused,
            DispatchCertainty::NotDelivered,
            DispatchCertainty::Unsettled,
            DispatchCertainty::Occurred,
        ])?;
        let error = BotError::DomainError {
            domain: "sim::domain".to_owned(),
            certainty,
            cause: format!("retry {text} transient permanent safe"),
        };
        assert_eq!(error.dispatch_certainty(), certainty, "seed {seed:#x}");
        assert_eq!(
            error.retry_class(),
            class_of(certainty),
            "seed {seed:#x}: {certainty:?}"
        );
        assert_eq!(certainty.retry_class(), class_of(certainty), "seed {seed:#x}");
        Ok(())
    })
}

#[test]
/// A refusal made before any dispatch is never retried, whatever it names.
fn sim_a_pre_dispatch_refusal_is_never_retried() -> Outcome {
    sweep(|seed, _, text| {
        let denied = GrantSet::empty()
            .admit(&[Cap::new(text.to_owned())])
            .err()
            .ok_or_else(|| format!("seed {seed:#x}: an empty set admitted {text:?}"))?;
        for error in [
            denied,
            BotError::UnregisteredDomain {
                domain: text.to_owned(),
            },
        ] {
            assert_eq!(
                error.dispatch_certainty(),
                DispatchCertainty::Refused,
                "seed {seed:#x}"
            );
            assert_eq!(error.retry_class(), RetryClass::Never, "seed {seed:#x}");
        }
        Ok(())
    })
}

/// The replay receipt of every rendering drawn under `seed`.
fn rendering_trace(seed: u64) -> Result<u64, String> {
    let mut stream = Stream::new(seed);
    let mut trace = RECEIPT_START;
    for _ in 0..DRAWS {
        let text = hostile(&mut stream)?;
        trace = fold(trace, Escaped(&text).to_string().as_bytes());
    }
    Ok(trace)
}

#[test]
/// The replay oracle: one seed, one rendering trace.
fn sim_the_same_seed_replays_the_same_renderings() -> Outcome {
    for seed in SEEDS {
        assert_eq!(
            rendering_trace(seed)?,
            rendering_trace(seed)?,
            "seed {seed:#x} replays"
        );
    }
    Ok(())
}

#[test]
/// The divergence oracle: a sweep that ignored its seed would pass the replay.
fn sim_distinct_seeds_draw_different_renderings() -> Outcome {
    assert_ne!(
        rendering_trace(SEEDS[0])?,
        rendering_trace(SEEDS[1])?,
        "two seeds drew alike"
    );
    Ok(())
}
