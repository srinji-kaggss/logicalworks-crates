//! Seeded deterministic replay of the `trace` debugger-configuration contracts.
//!
//! One seed draws every format spelling, service name, filter and builder
//! order, and the same seed must produce the same trace hash. Every probe drives
//! the public API of the shipped crate and is checked against a reference model
//! written here from the module's documentation, not from its source.
//!
//! Sources: the OpenTelemetry resource semantic conventions, which make
//! `service.name` the one required resource attribute (a nameless telemetry
//! source cannot be attributed), and the `tracing-subscriber` `EnvFilter`
//! directive grammar, under which an unbalanced span selector is a parse error.
//! Requirements proved: a config that cannot name its service is refused before
//! its filter is even parsed; a filter that cannot parse is refused before any
//! process-global subscriber is touched; format names are matched
//! case-insensitively and exactly; the builder is a pure value.
//!
//! No test here installs a subscriber: every `install` call is one the
//! contract refuses before `set_global_default`, which the refusal asserts.

use std::error::Error;

use crate::seeded_bytes::{below, fold_bytes, fold_refusal, next_byte, next_text};
use crate::seeded_sweep::{
    SWEEP_SEEDS, assert_distinct_seeds_diverge, assert_same_seed_replays, fold, fold_usize,
    initial_trace, seeded_stream,
};

use lgwks_std::seeded::Seeded;
use lgwks_std::trace::{
    DEFAULT_FILTER, DebugConfig, DebugFormat, DebugInstallError, LOG_FILTER_ENV, LOG_FORMAT_ENV,
    Level, OTEL_SCHEMA_URL, RUST_LOG_FILTER_ENV, event_enabled, info_span,
};

/// The documented format names, each beside the variant it must parse to.
const DOCUMENTED: [(&str, DebugFormat); 3] = [
    ("compact", DebugFormat::Compact),
    ("pretty", DebugFormat::Pretty),
    ("json", DebugFormat::Json),
];

/// The whitespace a blank service name is drawn from: what `str::trim` removes.
const BLANKS: [char; 5] = [' ', '\t', '\n', '\r', '\u{3000}'];

/// How many draws each family makes per seed.
const DRAWS: usize = 64;

/// The reference parser: a documented name, compared without regard to ASCII
/// case and with nothing trimmed, or no format at all.
fn reference_parse(value: &str) -> Option<DebugFormat> {
    let lowered = value.to_ascii_lowercase();
    DOCUMENTED
        .iter()
        .find(|entry| entry.0 == lowered)
        .map(|entry| entry.1)
}

/// The trace word a parse outcome folds as.
fn format_word(format: Option<DebugFormat>) -> u64 {
    match format {
        Some(DebugFormat::Compact) => 1,
        Some(DebugFormat::Pretty) => 2,
        Some(DebugFormat::Json) => 3,
        Some(_) => 4,
        None => 0,
    }
}

/// `name` with each ASCII letter's case drawn from `state`.
fn scrambled_case(state: &mut Seeded, name: &str) -> String {
    name.chars()
        .map(|character| {
            if next_byte(state) & 1 == 1 {
                character.to_ascii_uppercase()
            } else {
                character
            }
        })
        .collect()
}

/// A service name made only of the whitespace `trim` removes, possibly empty.
fn blank_name(state: &mut Seeded) -> String {
    let span = below(state, 12);
    (0..span)
        .map(|_| BLANKS[below(state, BLANKS.len())])
        .collect()
}

/// A service name with at least one visible character.
fn visible_name(state: &mut Seeded) -> String {
    let span = below(state, 24).saturating_add(1);
    let drawn = next_text(state, span);
    if drawn.trim().is_empty() {
        "svc".to_owned()
    } else {
        drawn
    }
}

/// A filter the `EnvFilter` grammar refuses: a span selector opened and never
/// closed, after a drawn target.
fn unbalanced_filter(state: &mut Seeded) -> String {
    let span = below(state, 10);
    let target: String = (0..span)
        .map(|_| char::from(b'a'.saturating_add(next_byte(state).rem_euclid(26))))
        .collect();
    let tail_len = below(state, 6);
    let tail = next_text(state, tail_len).replace(']', "");
    format!("{target}[{tail}")
}

/// A format drawn uniformly from the three documented ones.
fn drawn_format(state: &mut Seeded) -> DebugFormat {
    DOCUMENTED[below(state, DOCUMENTED.len())].1
}

/// The refusal a config `install` must return, as the trace arm it folds as.
fn refusal_arm(error: &DebugInstallError) -> u64 {
    match *error {
        DebugInstallError::MissingServiceName => 1,
        DebugInstallError::InvalidFilter { .. } => 2,
        DebugInstallError::InvalidFormat { .. } => 3,
        DebugInstallError::InvalidEnvironment { .. } => 4,
        DebugInstallError::InstallFailed { .. } => 5,
        _ => 6,
    }
}

/// Runs the seeded parse, builder and refusal sweep and returns its trace.
fn debug_trace(seed: u64) -> u64 {
    let mut state = seeded_stream(seed);
    let mut trace = initial_trace();
    for _ in 0..DRAWS {
        let name = DOCUMENTED[below(&mut state, DOCUMENTED.len())].0;
        let spelled = scrambled_case(&mut state, name);
        let parsed = DebugFormat::parse(&spelled);
        assert_eq!(
            parsed,
            reference_parse(&spelled),
            "seed {seed}: {spelled:?} parses as the reference says"
        );
        fold_bytes(&mut trace, spelled.as_bytes());
        fold(&mut trace, format_word(parsed));

        let junk_len = below(&mut state, 9);
        let junk = next_text(&mut state, junk_len);
        fold(&mut trace, format_word(DebugFormat::parse(&junk)));

        let service = blank_name(&mut state);
        let refused = DebugConfig::new(service.clone())
            .with_filter(unbalanced_filter(&mut state))
            .install();
        match refused {
            Err(ref error) => fold_refusal(&mut trace, refusal_arm(error), service.len(), 0),
            Ok(()) => fold(&mut trace, u64::MAX),
        }
    }
    trace
}

#[test]
/// Every case spelling of a documented name parses to its format, and the
/// format's stable name is the lowercase documented spelling.
fn every_case_spelling_of_a_documented_name_parses_to_its_format() {
    for seed in SWEEP_SEEDS {
        let mut state = seeded_stream(seed);
        for _ in 0..DRAWS {
            let (name, format) = DOCUMENTED[below(&mut state, DOCUMENTED.len())];
            let spelled = scrambled_case(&mut state, name);
            assert_eq!(
                DebugFormat::parse(&spelled),
                Some(format),
                "seed {seed}: {spelled:?} is a spelling of {name}"
            );
            assert_eq!(
                format.as_str(),
                name,
                "seed {seed}: {format:?} reports its documented name"
            );
        }
    }
}

#[test]
/// A drawn word is a format only when the reference says so: nothing outside
/// the three names is admitted.
fn a_drawn_word_parses_only_when_the_reference_admits_it() {
    for seed in SWEEP_SEEDS {
        let mut state = seeded_stream(seed);
        for _ in 0..DRAWS {
            let span = below(&mut state, 12);
            let word = next_text(&mut state, span);
            assert_eq!(
                DebugFormat::parse(&word),
                reference_parse(&word),
                "seed {seed}: {word:?} is judged as the reference judges it"
            );
        }
    }
}

#[test]
/// The match is exact: a documented name with drawn padding, a prefix or a
/// suffix is refused rather than trimmed into a format.
fn padding_prefixes_and_suffixes_are_refused_rather_than_trimmed() {
    for seed in SWEEP_SEEDS {
        let mut state = seeded_stream(seed);
        for _ in 0..DRAWS {
            let (name, _) = DOCUMENTED[below(&mut state, DOCUMENTED.len())];
            let pad = BLANKS[below(&mut state, BLANKS.len())];
            let extra = visible_name(&mut state);
            for variant in [
                format!("{pad}{name}"),
                format!("{name}{pad}"),
                format!("{name}{extra}"),
                format!("{extra}{name}"),
            ] {
                assert_eq!(
                    DebugFormat::parse(&variant),
                    None,
                    "seed {seed}: {variant:?} is not a documented name"
                );
            }
            for cut in 0..name.len() {
                if let Some(prefix) = name.get(..cut) {
                    assert_eq!(
                        DebugFormat::parse(prefix),
                        None,
                        "seed {seed}: {prefix:?} is a strict prefix of {name}"
                    );
                }
            }
        }
    }
}

#[test]
/// A fresh config names its service, the documented default filter, and
/// compact output, for every drawn service name.
fn a_fresh_config_carries_the_documented_defaults() {
    for seed in SWEEP_SEEDS {
        let mut state = seeded_stream(seed);
        for _ in 0..DRAWS {
            let service = visible_name(&mut state);
            let config = DebugConfig::new(service.clone());
            assert_eq!(
                config.service_name(),
                service,
                "seed {seed}: the service name is kept verbatim"
            );
            assert_eq!(
                config.filter(),
                DEFAULT_FILTER,
                "seed {seed}: the default filter is the documented one"
            );
            assert_eq!(
                config.format(),
                DebugFormat::Compact,
                "seed {seed}: compact is the default format"
            );
        }
    }
}

/// One drawn builder step applied to `config`, with the reference model of the
/// final settings updated alongside it.
fn apply_drawn_step(
    state: &mut Seeded,
    config: DebugConfig,
    filter: &mut String,
    format: &mut DebugFormat,
    flags: &mut [bool; 4],
) -> DebugConfig {
    match below(state, 6) {
        0 => {
            let span = below(state, 8);
            *filter = next_text(state, span);
            config.with_filter(filter.clone())
        }
        1 => {
            *format = drawn_format(state);
            config.with_format(*format)
        }
        slot => {
            let index = slot.saturating_sub(2);
            let value = next_byte(state) & 1 == 1;
            if let Some(flag) = flags.get_mut(index) {
                *flag = value;
            }
            match index {
                0 => config.with_ansi(value),
                1 => config.with_targets(value),
                2 => config.with_thread_names(value),
                _ => config.with_thread_ids(value),
            }
        }
    }
}

#[test]
/// The builder is a pure value: the last setting of each field wins, and two
/// configs given the same final settings in different orders are equal.
fn the_last_setting_wins_and_builder_order_does_not_matter() {
    for seed in SWEEP_SEEDS {
        let mut state = seeded_stream(seed);
        for _ in 0..DRAWS {
            let service = visible_name(&mut state);
            let mut forward = DebugConfig::new(service.clone());
            let mut filter = DEFAULT_FILTER.to_owned();
            let mut format = DebugFormat::Compact;
            let mut flags = [true, true, true, false];
            for _ in 0..below(&mut state, 12) {
                forward =
                    apply_drawn_step(&mut state, forward, &mut filter, &mut format, &mut flags);
            }
            assert_eq!(
                forward.filter(),
                filter,
                "seed {seed}: the last filter wins"
            );
            assert_eq!(
                forward.format(),
                format,
                "seed {seed}: the last format wins"
            );
            let reversed = DebugConfig::new(service)
                .with_thread_ids(flags[3])
                .with_thread_names(flags[2])
                .with_targets(flags[1])
                .with_ansi(flags[0])
                .with_format(format)
                .with_filter(filter);
            assert_eq!(
                forward, reversed,
                "seed {seed}: the same final settings in another order are one config"
            );
        }
    }
}

#[test]
/// A service name that trims to nothing is refused, and that refusal comes
/// before the filter is parsed: an unparseable filter on a blank name still
/// reports the missing name.
fn a_blank_service_name_is_refused_before_the_filter_is_parsed() {
    for seed in SWEEP_SEEDS {
        let mut state = seeded_stream(seed);
        for _ in 0..DRAWS {
            let service = blank_name(&mut state);
            let filter = unbalanced_filter(&mut state);
            let refused = DebugConfig::new(service.clone())
                .with_filter(filter)
                .with_format(drawn_format(&mut state))
                .install();
            assert!(
                matches!(refused, Err(DebugInstallError::MissingServiceName)),
                "seed {seed}: blank name {service:?} must be refused first: {refused:?}"
            );
        }
    }
}

#[test]
/// An unparseable filter on a named service is refused as a filter error that
/// carries the directive verbatim and its parser's cause.
fn an_unparseable_filter_is_refused_with_its_directive_and_cause() {
    for seed in SWEEP_SEEDS {
        let mut state = seeded_stream(seed);
        for _ in 0..DRAWS {
            let filter = unbalanced_filter(&mut state);
            let refused = DebugConfig::new(visible_name(&mut state))
                .with_filter(filter.clone())
                .install();
            let Err(error) = refused else {
                assert!(
                    refused.is_err(),
                    "seed {seed}: {filter:?} installed a global subscriber"
                );
                continue;
            };
            assert!(
                matches!(error, DebugInstallError::InvalidFilter { ref value, .. } if *value == filter),
                "seed {seed}: {filter:?} is refused as itself: {error:?}"
            );
            assert!(
                error.source().is_some(),
                "seed {seed}: the refusal carries the parser's cause"
            );
            assert_eq!(
                error.to_string(),
                format!("trace filter {filter:?} is invalid"),
                "seed {seed}: the message quotes the directive"
            );
        }
    }
}

#[test]
/// The format refusal names the variable and quotes the rejected value, and
/// neither it nor the missing-name refusal claims a cause it does not have.
fn refusals_name_their_variable_and_claim_no_invented_cause() {
    for seed in SWEEP_SEEDS {
        let mut state = seeded_stream(seed);
        for _ in 0..DRAWS {
            let span = below(&mut state, 10);
            let value = next_text(&mut state, span);
            let refusal = DebugInstallError::InvalidFormat {
                value: value.clone(),
            };
            assert_eq!(
                refusal.to_string(),
                format!("{LOG_FORMAT_ENV} value {value:?} is not one of compact, pretty, json"),
                "seed {seed}: the format refusal quotes its value"
            );
            assert!(
                refusal.source().is_none(),
                "seed {seed}: a format refusal has no underlying cause"
            );
        }
    }
    let missing = DebugInstallError::MissingServiceName;
    assert_eq!(
        missing.to_string(),
        "debugger service name is empty",
        "the missing-name refusal is stable text"
    );
    assert!(
        missing.source().is_none(),
        "the missing-name refusal has no underlying cause"
    );
}

#[test]
/// With no subscriber installed, emitting is a no-op the caller can observe:
/// no level is enabled and a span is disabled, at every drawn level.
fn with_no_subscriber_every_level_is_disabled() {
    for seed in SWEEP_SEEDS {
        let mut state = seeded_stream(seed);
        for _ in 0..DRAWS {
            let (level, enabled) = match below(&mut state, 5) {
                0 => (Level::ERROR, event_enabled!(Level::ERROR)),
                1 => (Level::WARN, event_enabled!(Level::WARN)),
                2 => (Level::INFO, event_enabled!(Level::INFO)),
                3 => (Level::DEBUG, event_enabled!(Level::DEBUG)),
                _ => (Level::TRACE, event_enabled!(Level::TRACE)),
            };
            assert!(
                !enabled,
                "seed {seed}: {level} is enabled with no subscriber installed"
            );
        }
    }
    assert!(
        info_span!("sim_trace_probe").is_disabled(),
        "a span with no subscriber is disabled"
    );
}

#[test]
/// The environment and schema names are the documented ones, so an operator
/// reading the docs sets the variable the code reads.
fn the_documented_environment_and_schema_names_are_the_ones_read() {
    let mut trace = initial_trace();
    for name in [
        LOG_FILTER_ENV,
        RUST_LOG_FILTER_ENV,
        LOG_FORMAT_ENV,
        DEFAULT_FILTER,
        OTEL_SCHEMA_URL,
    ] {
        fold_bytes(&mut trace, name.as_bytes());
        fold_usize(&mut trace, name.len());
    }
    assert_eq!(
        (LOG_FILTER_ENV, RUST_LOG_FILTER_ENV, LOG_FORMAT_ENV),
        ("LGWKS_LOG", "RUST_LOG", "LGWKS_LOG_FORMAT"),
        "the filter and format variables are the documented ones"
    );
    assert!(
        OTEL_SCHEMA_URL.starts_with("https://opentelemetry.io/schemas/"),
        "the schema URL is an OpenTelemetry schema"
    );
    assert_ne!(trace, initial_trace(), "the names fold into a trace");
}

#[test]
/// The replay oracle: one seed, one trace.
fn the_same_seed_replays_to_the_same_debug_trace() {
    for seed in SWEEP_SEEDS {
        assert_same_seed_replays(debug_trace, seed);
    }
}

#[test]
/// The divergence oracle: a sweep that ignored its seed would pass the replay.
fn distinct_debug_seeds_diverge_in_their_trace() {
    assert_distinct_seeds_diverge(debug_trace, SWEEP_SEEDS[0], SWEEP_SEEDS[1]);
}
