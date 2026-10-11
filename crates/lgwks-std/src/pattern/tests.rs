
use super::*;

fn config(input_limit: usize, output_limit: usize) -> PatternConfig {
    PatternConfig::with_limits(64 * 1024, 1_000_000, 250, input_limit, output_limit)
}

fn produced_matches() -> usize {
    PRODUCED_MATCHES.with(std::cell::Cell::get)
}

fn reset_produced_matches() {
    PRODUCED_MATCHES.with(|count| count.set(0));
}

/// Result and measured vector growth from the intentionally eager oracle.
struct EagerResult<'t> {
    /// Every produced match.
    matches: Vec<Match<'t>>,
    /// Number of capacity-growth allocation events observed.
    growth_events: usize,
    /// Capacity bytes acquired across those growth events.
    capacity_bytes: usize,
}

fn eager_mutant<'t>(re: &BoundedRegex, text: &'t str) -> EagerResult<'t> {
    let mut matches = Vec::new();
    let mut growth_events = 0usize;
    let mut capacity_bytes = 0usize;
    for matched in re.engine.find_iter(text) {
        count_produced_match();
        let old_capacity = matches.capacity();
        matches.push(convert_match(matched));
        let new_capacity = matches.capacity();
        if new_capacity > old_capacity {
            growth_events = growth_events.saturating_add(1);
            capacity_bytes = capacity_bytes.saturating_add(
                new_capacity
                    .saturating_sub(old_capacity)
                    .saturating_mul(std::mem::size_of::<Match<'t>>()),
            );
        }
    }
    EagerResult {
        matches,
        growth_events,
        capacity_bytes,
    }
}

#[test]
fn producer_counter_detects_eager_collection_mutant() -> Result<(), Box<dyn std::error::Error>> {
    let re = Regex::with_config(r"\d", config(32, 32))?;
    reset_produced_matches();
    let first = re.find_all("1x2x3x4x5x6x7x8")?.take(1).next();
    assert_eq!(
        first.map(|item| (item.text, item.start, item.end)),
        Some(("1", 0, 1))
    );
    assert_eq!(produced_matches(), 1);

    reset_produced_matches();
    let eager = eager_mutant(&re, "1x2x3x4x5x6x7x8");
    assert_eq!(eager.matches.len(), 8);
    assert_eq!(produced_matches(), 8);
    assert!(eager.growth_events > 0);
    assert!(eager.capacity_bytes >= 8 * std::mem::size_of::<Match<'_>>());
    Ok(())
}

#[test]
fn configured_limits_refuse_input_and_amplified_output() -> Result<(), Box<dyn std::error::Error>> {
    let bounded = Regex::with_config("a", config(2, 5))?;
    assert_eq!(
        bounded.is_match("aaa"),
        Err(PatternRunError::InputTooLarge {
            limit: 2,
            actual: 3
        })
    );
    let amplify = Regex::with_config("a", config(8, 5))?;
    assert_eq!(
        amplify.replace_all("aaa", "XX"),
        Err(PatternRunError::OutputTooLarge {
            limit: 5,
            attempted: 6
        })
    );
    Ok(())
}

/// The bounded expander is a second implementation of the engine's
/// replacement syntax, so it is held to byte-for-byte agreement with the
/// engine on the templates where a hand-written parser drifts: signed and
/// zero-padded group numbers, an index that overflows `usize`, names that
/// start with digits, and every unterminated or bare `$`.
#[test]
fn bounded_replacement_expands_exactly_like_the_engine() -> Result<(), Box<dyn std::error::Error>> {
    let pattern = r"(?<name>a)(b)";
    let unbounded = Regex::new(pattern)?;
    let bounded = Regex::with_config(pattern, config(64, 4096))?;
    for template in [
        "${+1}",
        "$1a",
        "${1}a",
        "$01",
        "${01}",
        "$",
        "a$",
        "$-",
        "${",
        "${unclosed",
        "$$1",
        "$$$1",
        "${name}",
        "$name",
        "$name$2",
        "$99999999999999999999",
        "${99999999999999999999}",
        "${ 1}",
        "${}",
        "$0$0",
        "é$2é",
    ] {
        assert_eq!(
            bounded.replace_all("abxab", template)?,
            unbounded.replace_all("abxab", template),
            "template {template:?} must expand as the engine expands it"
        );
    }
    Ok(())
}

#[test]
fn replacement_borrows_no_match_and_preserves_capture_rules()
-> Result<(), Box<dyn std::error::Error>> {
    let no_match = Regex::with_config("z", config(32, 32))?;
    let input = String::from("é-data");
    let output = no_match.replace_all(&input, "x")?;
    assert!(matches!(output, Cow::Borrowed(value) if value == input));
    let narrow_output = Regex::with_config("z", config(32, 3))?;
    assert_eq!(
        narrow_output.replace_all(&input, "x"),
        Err(PatternRunError::OutputTooLarge {
            limit: 3,
            attempted: input.len()
        })
    );

    let captures = Regex::with_config(r"(a)?()", config(32, 32))?
        .captures("b")?
        .ok_or("empty capture missing")?;
    assert_eq!(captures[1], None);
    assert_eq!(captures[2], Some(""));
    let expansion = Regex::with_config(r"(?<left>a)(?<right>b)", config(32, 32))?;
    assert_eq!(expansion.replace("ab", "${right}$left$$")?, "ba$");
    let unicode = Regex::with_config("é", config(32, 32))?
        .find("xéy")?
        .ok_or("match missing")?;
    assert_eq!((unicode.text, unicode.start, unicode.end), ("é", 1, 3));
    assert_eq!(
        Regex::with_config("", config(32, 32))?.replace_all("é", "-")?,
        "-é-"
    );
    Ok(())
}

#[test]
fn greedy_adversary_and_literal_control_keep_exact_match_workloads()
-> Result<(), Box<dyn std::error::Error>> {
    let adversary = Regex::with_config(r".*[^A-Z]|[A-Z]", config(16, 16))?;
    let spans = adversary
        .find_all("AAAA")?
        .map(|item| (item.start, item.end))
        .collect::<Vec<_>>();
    assert_eq!(spans, vec![(0, 1), (1, 2), (2, 3), (3, 4)]);
    assert_eq!(adversary.replace_all("AAAA", "X")?, "XXXX");
    assert_eq!(
        adversary.split("AAAA")?.collect::<Vec<_>>(),
        vec!["", "", "", "", ""]
    );

    let literal = Regex::with_config("A", config(16, 16))?;
    let literal_spans = literal
        .find_all("AAAA")?
        .map(|item| (item.start, item.end))
        .collect::<Vec<_>>();
    assert_eq!(literal_spans, vec![(0, 1), (1, 2), (2, 3), (3, 4)]);
    Ok(())
}

#[test]
fn configured_pattern_compile_size_and_nesting_limits_are_enforced()
-> Result<(), Box<dyn std::error::Error>> {
    let source_error =
        Regex::with_config("long", PatternConfig::with_limits(3, 1_000, 250, 16, 16))
            .err()
            .ok_or("source pattern limit was ignored")?;
    assert_eq!(
        source_error.kind(),
        PatternErrorKind::PatternTooLarge {
            limit: 3,
            actual: 4
        }
    );
    assert_eq!(source_error.pattern(), "long");
    let large_source = "x".repeat(1_000);
    let excerpt_error = Regex::with_config(
        &large_source,
        PatternConfig::with_limits(3, 1_000, 250, 16, 16),
    )
    .err()
    .ok_or("oversize source pattern was compiled")?;
    assert_eq!(
        excerpt_error.kind(),
        PatternErrorKind::PatternTooLarge {
            limit: 3,
            actual: 1_000
        }
    );
    assert!(excerpt_error.pattern().len() <= 64);

    let compile_error = Regex::with_config(r"\w", PatternConfig::with_limits(1024, 1, 250, 16, 16))
        .err()
        .ok_or("compiled-size limit was ignored")?;
    assert_eq!(compile_error.kind(), PatternErrorKind::CompiledTooLarge);

    let nesting_error =
        Regex::with_config("ab", PatternConfig::with_limits(1024, 10_000, 0, 16, 16))
            .err()
            .ok_or("nesting limit was ignored")?;
    assert_eq!(nesting_error.kind(), PatternErrorKind::Syntax);
    Ok(())
}

#[test]
fn compiles_valid_pattern() {
    assert!(Regex::new(r"\d+").is_ok());
}

#[test]
fn rejects_invalid_pattern() {
    assert!(Regex::new(r"[unclosed").is_err());
}

// A test that must observe a refusal returns `Result` and binds the
// refusal explicitly, so a successful compile fails the test naming the
// expectation it broke instead of panicking inside an `unwrap_err`.
#[test]
fn error_display_escapes_control_characters() -> Result<(), Box<dyn std::error::Error>> {
    // An invalid pattern containing a newline must render as an escaped
    // `\n`, not a raw byte that forges a second line in a caller's log.
    let Err(error) = Regex::new("(\n") else {
        return Err("a pattern with an unclosed group must not compile".into());
    };
    let rendered = error.to_string();
    assert!(
        !rendered.contains('\n'),
        "raw newline survived: {rendered:?}"
    );
    assert!(rendered.contains("invalid pattern"));
    let valid_control_pattern = Regex::with_config("[\n]", PatternConfig::default())?;
    let debug = format!("{valid_control_pattern:?}");
    assert!(!debug.contains('\n'));
    assert!(debug.contains("\\n"));
    Ok(())
}

#[test]
fn is_match_finds_substring() -> Result<(), PatternError> {
    let re = Regex::new(r"\d+")?;
    assert!(re.is_match("abc123def"));
    assert!(!re.is_match("abcdef"));
    Ok(())
}

#[test]
fn find_returns_first_match() -> Result<(), Box<dyn std::error::Error>> {
    let re = Regex::new(r"\d+")?;
    let matched = re
        .find("abc123def456")
        .ok_or("the digit pattern must match abc123def456")?;
    assert_eq!(matched.text, "123");
    assert_eq!(matched.start, 3);
    assert_eq!(matched.end, 6);
    Ok(())
}

#[test]
fn find_all_returns_every_match() -> Result<(), PatternError> {
    let re = Regex::new(r"\d+")?;
    let matches: Vec<Match<'_>> = re.find_all("a1b22c333").collect();
    assert_eq!(matches.len(), 3);
    assert_eq!(matches[0].text, "1");
    assert_eq!(matches[1].text, "22");
    assert_eq!(matches[2].text, "333");
    Ok(())
}

/// The iterator is lazy, so stopping early leaves the rest of the haystack
/// unvisited. Liveness is observed through a counter the closure
/// increments: `.take(1)` must yield one match and the predicate must run
/// for one match only, which a collected `Vec` could not have shown.
#[test]
fn find_all_stops_where_the_caller_stops() -> Result<(), PatternError> {
    let re = Regex::new(r"\d+")?;
    let mut seen = 0usize;
    let first = re
        .find_all("a1b22c333d4444")
        .inspect(|_| seen = seen.saturating_add(1))
        .take(1)
        .next();
    assert_eq!(first.map(|matched| matched.text), Some("1"));
    assert_eq!(seen, 1);
    Ok(())
}

#[test]
fn captures_extracts_groups() -> Result<(), Box<dyn std::error::Error>> {
    let re = Regex::new(r"(\w+)@(\w+)\.(\w+)")?;
    let captures = re
        .captures("user@host.com")
        .ok_or("the three-group pattern must match user@host.com")?;
    assert_eq!(captures[1], Some("user"));
    assert_eq!(captures[2], Some("host"));
    assert_eq!(captures[3], Some("com"));
    Ok(())
}

#[test]
fn replace_substitutes_first() -> Result<(), PatternError> {
    let re = Regex::new(r"\d+")?;
    assert_eq!(re.replace("a1b2c3", "X"), "aXb2c3");
    Ok(())
}

#[test]
fn replace_all_substitutes_every_match() -> Result<(), PatternError> {
    let re = Regex::new(r"\d+")?;
    assert_eq!(re.replace_all("a1b2c3", "X"), "aXbXcX");
    Ok(())
}

#[test]
fn split_divides_on_pattern() -> Result<(), PatternError> {
    let re = Regex::new(r"[,;]\s*")?;
    assert_eq!(
        re.split("a, b; c,d").collect::<Vec<&str>>(),
        vec!["a", "b", "c", "d"]
    );
    Ok(())
}

#[test]
fn error_includes_pattern_text() -> Result<(), Box<dyn std::error::Error>> {
    let Err(error) = Regex::new(r"(unclosed") else {
        return Err("a pattern with an unclosed group must not compile".into());
    };
    assert!(error.pattern().contains("unclosed"));
    assert!(!error.message().is_empty());
    Ok(())
}
