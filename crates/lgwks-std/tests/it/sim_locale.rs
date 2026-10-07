//! Locale independence of every `lgwks_std` reader of human text (#278 row 4).
//!
//! A parser that reads a timestamp, a hex digest, a percent-escaped component
//! or a JSON/RON number must give the same answer whatever `LC_ALL`, `LANG` and
//! `TZ` the process was started under, and must refuse the spellings another
//! locale would write: Arabic-Indic and Persian digits, a decimal comma, a
//! grouping separator. Three properties, each driven through the public API:
//!
//! 1. One seed draws a case per family — an instant anywhere in the RFC 3339
//!    year range, a byte payload, a component in several scripts, a number —
//!    and the case round-trips, every localized spelling of it is refused, and
//!    the same seeds replay to the same trace hash.
//! 2. The whole sweep, re-run in a child of this test binary under each locale
//!    in [`LOCALES`], reads to the trace this process read under its own.
//! 3. A process start token read by [`identify_process`] in each of those
//!    children names the same start as the token read here. This is the one
//!    reader that goes outside the process: `ps -o lstart` prints its date in
//!    the caller's language and time zone (measured on macOS: `Mi.  7 Okt.`
//!    under `de_DE`, Arabic and Persian digits under `ar_SA` and `fa_IR`, a
//!    `+0330` offset under `Asia/Tehran`), so the crate pins `TZ=UTC0` and
//!    `LC_ALL=C` on the one command that runs it. Removing that pin fails this
//!    family.
//!
//! **Host limit.** The in-process readers are locale-independent by
//! construction — the Rust standard library never calls `setlocale`, and no
//! reader here formats through libc — so property 2 is an external observation
//! of that construction rather than the only thing holding it. A host without
//! one of the locales falls back to `C` for it, which makes that locale's row
//! vacuous rather than failing; the runners this repository gates on are macOS
//! hosts that carry all five.

use std::error::Error;
use std::io::Write as _;
use std::process::Command;

use lgwks_std::encoding::percent;
use lgwks_std::hex;
use lgwks_std::seeded::Seeded;
use lgwks_std::time::format::{from_unix_parts, to_rfc3339, unix_parts};
use lgwks_std::time::parse_rfc3339;

use crate::seeded_sweep::{fold, fold_usize, initial_trace};

type TestResult = Result<(), Box<dyn Error>>;

/// How many tests the sweep is split across, so one slow seed names its band.
const BANDS: u64 = 8;

/// Seeds in one band.
const BAND_WIDTH: u64 = 125;

/// Seeds the whole sweep covers: every band's, once.
const SEEDS: u64 = BANDS * BAND_WIDTH;

/// `0001-01-01T00:00:00Z`, the first instant RFC 3339 can spell.
const FIRST_SECOND: i64 = -62_135_596_800;

/// `9999-12-31T23:59:59Z`, the last instant RFC 3339 can spell.
const LAST_SECOND: i64 = 253_402_300_799;

/// Set in a child: print what this test read so the parent can compare.
const PROBE: &str = "LGWKS_LOCALE_PROBE";

/// Set in a child: the pid whose start the child reads.
const PROBE_PID: &str = "LGWKS_LOCALE_PROBE_PID";

/// The `(LC_ALL, TZ)` pairs every child runs under: the C locale, a decimal
/// comma, two scripts with their own digits, and a CJK calendar, each in a zone
/// whose offset is not zero (Tehran's is not a whole hour).
const LOCALES: [(&str, &str); 5] = [
    ("C", "UTC0"),
    ("de_DE.UTF-8", "Europe/Berlin"),
    ("ar_SA.UTF-8", "Asia/Riyadh"),
    ("fa_IR.UTF-8", "Asia/Tehran"),
    ("ja_JP.UTF-8", "Asia/Tokyo"),
];

/// U+0660 ARABIC-INDIC DIGIT ZERO; the nine digits after it are contiguous.
const ARABIC_INDIC_ZERO: u32 = 0x0660;

/// U+06F0 EXTENDED ARABIC-INDIC DIGIT ZERO, the digits Persian writes.
const PERSIAN_ZERO: u32 = 0x06F0;

/// Components in the scripts a localized caller hands a percent encoder.
const SCRIPTS: [&str; 6] = ["3,14", "١٬٢٣٤٫٥", "۱۲۳۴", "Straße", "令和七年", "1.234,5"];

/// Numbers as a localized formatter writes them; no JSON or RON reader may
/// take any of them for a number.
#[cfg(any(feature = "json", feature = "ron"))]
const LOCALIZED: [&str; 5] = ["3,14", "1.234,5", "١٢٣", "۳٫۱۴", "1 234"];

/// Folds a text's bytes and its length into the trace.
fn fold_text(trace: &mut u64, text: &str) {
    fold_usize(trace, text.len());
    for byte in text.bytes() {
        fold(trace, u64::from(byte));
    }
}

/// A value drawn uniformly from `low..=high`.
fn between(rng: &mut Seeded, low: i64, high: i64) -> Result<i64, Box<dyn Error>> {
    let span = u64::try_from(high.checked_sub(low).ok_or("the range is ordered")?)?;
    let offset = i64::try_from(rng.below(span.checked_add(1).ok_or("the span fits")?)?)?;
    Ok(low
        .checked_add(offset)
        .ok_or("the draw lies inside the range")?)
}

/// `text` with its `nth` ASCII digit written in the script whose zero is
/// `zero`, or `None` when the text has no `nth` digit.
fn localize_digit(text: &str, nth: usize, zero: u32) -> Option<String> {
    let mut seen = 0_usize;
    let mut replaced = false;
    let mut localized = String::with_capacity(text.len().saturating_add(2));
    for character in text.chars() {
        if character.is_ascii_digit() && seen == nth {
            let value = character.to_digit(10)?;
            localized.push(char::from_u32(zero.checked_add(value)?)?);
            replaced = true;
        } else {
            localized.push(character);
        }
        if character.is_ascii_digit() {
            seen = seen.saturating_add(1);
        }
    }
    replaced.then_some(localized)
}

/// How many ASCII digits `text` carries.
fn digits_in(text: &str) -> usize {
    text.bytes().filter(u8::is_ascii_digit).count()
}

/// One seed's case: every family's round trip and every localized refusal,
/// folded into `trace`.
fn locale_case(seed: u64, trace: &mut u64) -> TestResult {
    let mut rng = Seeded::from_seed(seed);

    // An instant anywhere RFC 3339 can spell, to the nanosecond.
    let seconds = between(&mut rng, FIRST_SECOND, LAST_SECOND)?;
    let nanos = u32::try_from(rng.below(1_000_000_000)?)?;
    let text = to_rfc3339(from_unix_parts(seconds, nanos)?)?;
    let read = unix_parts(parse_rfc3339(&text)?)?;
    assert_eq!(
        read,
        (seconds, nanos),
        "seed {seed:#018x}: {text} did not read back as the instant that wrote it"
    );
    fold_text(trace, &text);
    let nth = rng.index(digits_in(&text))?;
    for zero in [ARABIC_INDIC_ZERO, PERSIAN_ZERO] {
        let localized = localize_digit(&text, nth, zero).ok_or("the drawn digit exists")?;
        assert!(
            parse_rfc3339(&localized).is_err(),
            "seed {seed:#018x}: {localized} was read as a timestamp"
        );
    }
    if text.contains('.') {
        let comma = text.replacen('.', ",", 1);
        assert!(
            parse_rfc3339(&comma).is_err(),
            "seed {seed:#018x}: the decimal comma in {comma} was read as a fraction"
        );
    }

    // A byte payload through hex.
    let length = rng.index(32)?.saturating_add(1);
    let payload: Vec<u8> = (0..length)
        .map(|_| rng.next_u64().to_le_bytes()[0])
        .collect();
    let encoded = hex::encode(&payload);
    assert_eq!(
        hex::decode(&encoded)?,
        payload,
        "seed {seed:#018x}: {encoded} did not decode to its payload"
    );
    fold_text(trace, &encoded);
    if digits_in(&encoded) > 0 {
        let nth = rng.index(digits_in(&encoded))?;
        let localized =
            localize_digit(&encoded, nth, ARABIC_INDIC_ZERO).ok_or("the drawn digit exists")?;
        assert!(
            hex::decode(localized.as_bytes()).is_err(),
            "seed {seed:#018x}: {localized} was decoded as hex"
        );
    }

    // A component in another script through percent encoding.
    let script = SCRIPTS[rng.index(SCRIPTS.len())?];
    let component = format!("{script}{}", rng.next_u64());
    let escaped = percent::encode_component(&component);
    assert_eq!(
        percent::decode(&escaped)?,
        component,
        "seed {seed:#018x}: {escaped} did not decode to {component}"
    );
    fold_text(trace, &escaped);

    #[cfg(any(feature = "json", feature = "ron"))]
    numbers(seed, &mut rng, trace)?;
    Ok(())
}

/// The JSON and RON number readers: a dyadic fraction with one exact decimal
/// spelling round-trips to its bits, a whole `i64` round-trips, and a localized
/// spelling is not a number.
#[cfg(any(feature = "json", feature = "ron"))]
fn numbers(seed: u64, rng: &mut Seeded, trace: &mut u64) -> TestResult {
    let numerator = i32::from_le_bytes(rng.next_u64().to_le_bytes()[..4].try_into()?);
    let shift = u32::try_from(rng.below(11)?)?;
    let denominator = f64::from(1_u16.checked_shl(shift).ok_or("the shift fits")?);
    let value = f64::from(numerator) / denominator;
    let whole = i64::from_le_bytes(rng.next_u64().to_le_bytes());
    #[cfg(feature = "json")]
    {
        let text = lgwks_std::json::to_string(&value)?;
        let back: f64 = lgwks_std::json::from_str(&text)?;
        assert_eq!(
            back.to_bits(),
            value.to_bits(),
            "seed {seed:#018x}: JSON {text} did not read back as {value}"
        );
        fold_text(trace, &text);
        let text = lgwks_std::json::to_string(&whole)?;
        let back: i64 = lgwks_std::json::from_str(&text)?;
        assert_eq!(back, whole, "seed {seed:#018x}: JSON {text} lost its value");
        fold_text(trace, &text);
        for spelling in LOCALIZED {
            assert!(
                lgwks_std::json::from_str::<f64>(spelling).is_err(),
                "JSON read the localized {spelling} as a number"
            );
        }
    }
    #[cfg(feature = "ron")]
    {
        let text = lgwks_std::ron::to_string(&value)?;
        let back: f64 = lgwks_std::ron::from_str(&text)?;
        assert_eq!(
            back.to_bits(),
            value.to_bits(),
            "seed {seed:#018x}: RON {text} did not read back as {value}"
        );
        fold_text(trace, &text);
        let text = lgwks_std::ron::to_string(&whole)?;
        let back: i64 = lgwks_std::ron::from_str(&text)?;
        assert_eq!(back, whole, "seed {seed:#018x}: RON {text} lost its value");
        fold_text(trace, &text);
        for spelling in LOCALIZED {
            assert!(
                lgwks_std::ron::from_str::<f64>(spelling).is_err(),
                "RON read the localized {spelling} as a number"
            );
        }
    }
    Ok(())
}

/// The trace of every seed in `first..end`.
fn sweep(first: u64, end: u64) -> Result<u64, Box<dyn Error>> {
    let mut trace = initial_trace();
    for seed in first..end {
        locale_case(seed, &mut trace)?;
    }
    Ok(trace)
}

/// One band of the sweep, run twice: the same seeds must read the same trace.
fn locale_band(index: u64) -> TestResult {
    let first = index.checked_mul(BAND_WIDTH).ok_or("the band fits")?;
    let end = first.checked_add(BAND_WIDTH).ok_or("the band fits")?;
    assert_eq!(
        sweep(first, end)?,
        sweep(first, end)?,
        "band {index}: the same seeds read two different traces"
    );
    Ok(())
}

macro_rules! locale_family {
    ($($name:ident => $index:expr),+ $(,)?) => {
        $(
            #[test]
            fn $name() -> TestResult {
                locale_band($index)
            }
        )+
    };
}

locale_family! {
    locale_band_00 => 0,
    locale_band_01 => 1,
    locale_band_02 => 2,
    locale_band_03 => 3,
    locale_band_04 => 4,
    locale_band_05 => 5,
    locale_band_06 => 6,
    locale_band_07 => 7,
}

/// Writes `<label> <value>` for a parent to read; the value holds no space.
fn report(label: &str, value: &str) -> TestResult {
    let mut out = std::io::stdout().lock();
    writeln!(out, "{label} {value}")?;
    out.flush()?;
    Ok(())
}

/// Runs `test` from this binary in a child under `locale` and `zone`, and
/// returns the value it reported under `label`.
fn child(
    test: &str,
    (locale, zone): (&str, &str),
    pid: Option<u32>,
    label: &str,
) -> Result<String, Box<dyn Error>> {
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env("LC_ALL", locale)
        .env("LANG", locale)
        .env("TZ", zone)
        .env(PROBE, "1");
    if let Some(pid) = pid {
        command.env(PROBE_PID, pid.to_string());
    }
    let output = command.output()?;
    let stdout = String::from_utf8(output.stdout)?;
    // libtest writes `test <name> ... ` before the test's own output on the
    // same line, so the value is the word after the label wherever it falls.
    let mut words = stdout.split_whitespace();
    let reported = words
        .by_ref()
        .find(|word| *word == label)
        .and_then(|_| words.next())
        .map(str::to_owned);
    match reported {
        Some(value) if output.status.success() => Ok(value),
        _ => {
            let refusal = Err(format!(
                "{test} under {locale}/{zone} exited {} without reporting {label}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
            #[cfg(feature = "trace")]
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "child: returning an error to the caller");
            refusal
        }
    }
}

#[test]
/// The whole sweep replays in one process; a child reports its trace.
fn the_whole_sweep_replays_in_one_process() -> TestResult {
    let trace = sweep(0, SEEDS)?;
    assert_eq!(trace, sweep(0, SEEDS)?, "the whole sweep read two traces");
    if std::env::var_os(PROBE).is_some() {
        report("locale-trace", &format!("{trace:016x}"))?;
    }
    Ok(())
}

#[test]
/// Every locale's child reads the sweep to this process's trace.
fn the_sweep_reads_the_same_under_every_locale() -> TestResult {
    let here = format!("{:016x}", sweep(0, SEEDS)?);
    for pair in LOCALES {
        let there = child(
            "sim_locale::the_whole_sweep_replays_in_one_process",
            pair,
            None,
            "locale-trace",
        )?;
        assert_eq!(
            there, here,
            "under {}/{} the sweep read a different trace",
            pair.0, pair.1
        );
    }
    Ok(())
}

#[cfg(all(unix, feature = "process"))]
#[test]
/// A start token reads back through its own constructor; a child reports it.
fn a_process_start_is_read_in_this_process() -> TestResult {
    use lgwks_std::process::{ProcessIdentity, identify_process};

    let pid = match std::env::var_os(PROBE_PID) {
        Some(text) => text
            .to_str()
            .ok_or("the probe pid is text")?
            .parse::<i32>()?,
        None => i32::try_from(std::process::id())?,
    };
    let identity = identify_process(pid)?.ok_or("the probed process holds its pid")?;
    assert_eq!(
        ProcessIdentity::new(pid, identity.started())?,
        identity,
        "the start token {} did not rebuild its identity",
        identity.started()
    );
    if std::env::var_os(PROBE).is_some() {
        report("locale-start", identity.started())?;
    }
    Ok(())
}

#[cfg(all(unix, feature = "process"))]
#[test]
/// Every locale's child reads this process's start as the same token.
fn a_process_start_reads_the_same_under_every_locale() -> TestResult {
    use lgwks_std::process::identify_process;

    let pid = std::process::id();
    let here = identify_process(i32::try_from(pid)?)?.ok_or("this process holds its pid")?;
    for pair in LOCALES {
        let there = child(
            "sim_locale::a_process_start_is_read_in_this_process",
            pair,
            Some(pid),
            "locale-start",
        )?;
        assert_eq!(
            there,
            here.started(),
            "under {}/{} this process's start read as a different token",
            pair.0,
            pair.1
        );
    }
    Ok(())
}
