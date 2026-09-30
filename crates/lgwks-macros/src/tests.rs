//! The language, checked on source text without a compiler round trip.
//!
//! `proc_macro2` parses source text outside a real macro expansion and, with
//! span locations on, keeps each token's line and column. That lets every
//! refusal be asserted by its exact message: a `compile_fail` doctest passes
//! on any compile error at all, and so cannot tell a refusal from a typo.

use std::str::FromStr;

use lgwks_deps::proc_macro2::TokenStream;

use crate::{emit, lines};

/// Expand `source` as the macro would, as text or as the refusal message.
fn expand(source: &str) -> Result<String, String> {
    let stream = TokenStream::from_str(source).map_err(|error| error.to_string())?;
    lines::tree(lines::split(stream))
        .and_then(emit::script)
        .map(|tokens| tokens.to_string())
        .map_err(|error| error.to_string())
}

/// Every refusal the language makes, each with the fragment of its message
/// that names the replacement.
const REFUSALS: [(&str, &str, &str); 20] = [
    (
        "each without a bound",
        "flow f(xs: Vec<u8>):\n    each x in xs:\n        x\n",
        "each` needs a bound",
    ),
    (
        "zero fan-out",
        "flow f(xs: Vec<u8>):\n    each x in xs, at most 0 at once:\n        x\n",
        "outside 1..=65536",
    ),
    (
        "fan-out past the ceiling",
        "flow f(xs: Vec<u8>):\n    each x in xs, at most 70000 at once:\n        x\n",
        "outside 1..=65536",
    ),
    (
        "zero attempts",
        "flow f():\n    retry up to 0 times:\n        1\n",
        "outside 1..=1000",
    ),
    (
        "zero deadline",
        "flow f():\n    within 0s:\n        1\n",
        "zero duration",
    ),
    (
        "duration without a unit",
        "flow f():\n    within 5:\n        1\n",
        "number with a unit",
    ),
    (
        "unwrap",
        "flow f(text: String):\n    let n: u8 = text.parse().unwrap()\n",
        "`.unwrap()` ends the program",
    ),
    (
        "expect",
        "flow f(text: String):\n    let n: u8 = text.parse().expect(\"n\")\n",
        "`.expect()` ends the program",
    ),
    (
        "panic",
        "flow f():\n    panic!(\"no\")\n",
        "`panic!` ends the program",
    ),
    (
        "unbounded loop",
        "flow f():\n    loop { }\n",
        "a loop with no bound",
    ),
    (
        "while",
        "flow f():\n    while true { }\n",
        "a loop with no bound",
    ),
    (
        "spawn",
        "flow f():\n    tokio::spawn(async {})\n",
        "a spawned task has no owner",
    ),
    (
        "thread sleep",
        "flow f():\n    std::thread::sleep(d)\n",
        "`thread::sleep` stalls",
    ),
    (
        "machine path",
        "flow f():\n    let p = \"/Users/me/data.csv\"\n",
        "absolute path from one machine",
    ),
    (
        "give back from a nested block",
        "flow f() -> u8:\n    within 1s:\n        give back 1\n",
        "`give back` returns from the flow",
    ),
    (
        "promised output never produced",
        "flow f() -> u8:\n    let x = 1\n",
        "promises an output",
    ),
    (
        "stray indent",
        "flow f():\n    let x = 1\n        let y = 2\n",
        "unexpected indent",
    ),
    (
        "header with no block",
        "flow f():\n    within 1s:\nflow g():\n    1\n",
        "nothing is indented beneath it",
    ),
    (
        "unknown block",
        "flow f():\n    whenever x:\n        1\n",
        "unknown block",
    ),
    (
        "else without if",
        "flow f():\n    else:\n        1\n",
        "`else:` must follow",
    ),
];

#[test]
fn every_refusal_names_its_replacement() -> Result<(), String> {
    for (case, source, expected) in REFUSALS {
        let Err(message) = expand(source) else {
            return Err(format!("{case}: expanded instead of refusing"));
        };
        assert!(
            message.contains(expected),
            "{case}: expected {expected:?}, got {message:?}"
        );
    }
    Ok(())
}

#[test]
fn a_valid_script_expands_to_calls_into_the_runtime_and_a_map() -> Result<(), String> {
    let source = "pub flow crawl(paths: Vec<String>) -> Vec<usize>:\n\
                  \x20   let sizes = each path in paths, at most 16 at once:\n\
                  \x20       retry up to 3 times, waiting 200ms:\n\
                  \x20           within 1.5s:\n\
                  \x20               path.len()\n\
                  \x20   give back sizes\n";
    let expanded = expand(source)?;
    for fragment in [
        ":: lgwks_bot :: script :: each",
        ":: lgwks_bot :: script :: retry",
        ":: lgwks_bot :: script :: within",
        "from_nanos (1500000000)",
        "from_nanos (200000000)",
        "\"each:path\"",
        "pub const ARCHITECTURE",
        "StepKind :: Each",
    ] {
        assert!(
            expanded.contains(fragment),
            "missing {fragment:?} in {expanded}"
        );
    }
    Ok(())
}

#[test]
fn step_labels_carry_structure_never_line_numbers() -> Result<(), String> {
    let near = expand("flow f(xs: Vec<u8>):\n    each x in xs, at most 2 at once:\n        x\n")?;
    let far =
        expand("\n\n\n\nflow f(xs: Vec<u8>):\n    each x in xs, at most 2 at once:\n        x\n")?;
    let label = |text: &str| {
        text.split("\"each:")
            .nth(1)
            .map(|rest| rest.split('"').next().map(str::to_owned))
    };
    assert_eq!(
        label(&near),
        label(&far),
        "moving a flow down the file must not change its keys"
    );
    Ok(())
}

#[test]
fn sibling_steps_of_one_kind_get_distinct_labels() -> Result<(), String> {
    let expanded = expand(
        "flow f():\n    retry up to 2 times:\n        1\n    retry up to 2 times:\n        2\n",
    )?;
    assert!(
        expanded.contains("\"retry\""),
        "the first sibling keeps the plain label"
    );
    assert!(
        expanded.contains("\"retry~2\""),
        "the second is numbered, so their keys differ"
    );
    Ok(())
}
