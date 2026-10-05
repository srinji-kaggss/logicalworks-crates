//! External consumer coverage for the public pattern facade.
#![cfg(feature = "pattern")]

use std::borrow::Cow;

use lgwks_std::pattern::{PatternConfig, PatternRunError, Regex};

#[test]
fn bounded_pattern_api_returns_borrowed_data_and_typed_refusals()
-> Result<(), Box<dyn std::error::Error>> {
    let config = PatternConfig::with_limits(1024, 1_000_000, 250, 16, 8);
    let legacy = Regex::new("x")?;
    assert!(legacy.is_match("example"));
    let rejected = Regex::new("[").err().ok_or("invalid pattern compiled")?;
    assert_eq!(rejected.pattern(), "[");
    assert!(!rejected.message().is_empty());
    assert_eq!(
        rejected.kind(),
        lgwks_std::pattern::PatternErrorKind::Syntax
    );
    let regex = Regex::with_config("z", config)?;
    let input = String::from("abc");
    let result = regex.replace_all(&input, "x")?;
    assert!(matches!(result, Cow::Borrowed(value) if value == input));
    assert_eq!(
        regex.is_match("12345678901234567"),
        Err(PatternRunError::InputTooLarge {
            limit: 16,
            actual: 17
        })
    );

    let finds = Regex::with_config(r"\d+", config)?;
    let found = finds.find_all("a1b22")?.collect::<Vec<_>>();
    assert_eq!(
        found
            .iter()
            .map(|item| (item.start, item.end))
            .collect::<Vec<_>>(),
        vec![(1, 2), (3, 5)]
    );
    Ok(())
}
