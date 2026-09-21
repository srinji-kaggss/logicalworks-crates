//! RON as a flow document, and the one validation path both notations share.
//!
//! RON is the estate's notation for human-facing documents, and a flow is one.
//! What these tests hold is the part that is easy to get wrong when a second
//! codec arrives: that the notation changes the *spelling* only, and never
//! which documents are acceptable.

use lgwks_bot::{BotError, FlowSpec};

/// A minimal valid flow, in each notation.
const RON: &str = r#"FlowSpec(
    vars: {},
    entry: "end",
    nodes: { "end": (kind: "end") },
)"#;

const JSON: &str = r#"{
    "vars": {},
    "entry": "end",
    "nodes": { "end": { "kind": "end" } }
}"#;

/// The refusal each notation produced for its own spelling, so a test can
/// compare them.
///
/// Returned rather than matched so the assertions below carry the whole
/// refusal in their message.
fn refusals(ron: &str, json: &str) -> [Option<BotError>; 2] {
    [
        FlowSpec::from_ron(ron).err(),
        FlowSpec::from_json(json).err(),
    ]
}

/// The `(node, kind)` an unknown-node-kind refusal named, if that is what it was.
fn unknown_kind(source: &str) -> Option<(String, String)> {
    match FlowSpec::from_ron(source).err() {
        Some(BotError::UnknownNodeKind { node, kind }) => Some((node, kind)),
        _ => None,
    }
}

/// The `(bytes, limit)` an oversize refusal named, if that is what it was.
fn too_large(source: &str) -> Option<(usize, usize)> {
    match FlowSpec::from_ron(source).err() {
        Some(BotError::FlowTooLarge { bytes, limit }) => Some((bytes, limit)),
        _ => None,
    }
}

/// The diagnostic a malformed refusal carried, if that is what it was.
fn malformed(source: &str) -> Option<String> {
    match FlowSpec::from_ron(source).err() {
        Some(BotError::MalformedFlow { cause }) => Some(cause),
        _ => None,
    }
}

#[test]
fn ron_decodes_a_flow() -> Result<(), Box<dyn std::error::Error>> {
    let spec = FlowSpec::from_ron(RON)?;
    assert_eq!(spec.entry(), "end");
    assert_eq!(spec.nodes().len(), 1);
    Ok(())
}

/// The two notations are one document.
///
/// This is the property the shared validation exists for. If these ever
/// disagree, one notation has grown a rule the other does not have, and a
/// document's acceptability has started to depend on how it is spelled.
#[test]
fn both_notations_produce_the_same_flow() -> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(FlowSpec::from_ron(RON)?, FlowSpec::from_json(JSON)?);
    Ok(())
}

#[test]
fn a_flow_round_trips_through_ron() -> Result<(), Box<dyn std::error::Error>> {
    let spec = FlowSpec::from_json(JSON)?;
    let rendered = spec.to_ron()?;
    assert_eq!(FlowSpec::from_ron(&rendered)?, spec);
    Ok(())
}

/// RON is chosen for its ergonomics, so the ergonomics are pinned.
///
/// A comment and a trailing comma are the two things a hand-written document
/// needs most and JSON cannot have, and they are why `lgwks_std::ron` exists.
#[test]
fn ron_accepts_comments_and_trailing_commas() -> Result<(), Box<dyn std::error::Error>> {
    let annotated = r#"
// The review journey, one node for now.
FlowSpec(
    vars: {},
    entry: "end",
    nodes: {
        "end": (kind: "end"),   // the only node
    },
)
"#;
    assert_eq!(FlowSpec::from_ron(annotated)?, FlowSpec::from_json(JSON)?);
    Ok(())
}

/// An unknown node kind is refused by name, in either notation.
///
/// The diagnostic is why `reject_unknown_node_kinds` runs before serde: serde
/// reports an unknown variant without saying which node carried it, and an
/// operator reading a log for a bot that has been up for days needs the node.
#[test]
fn an_unknown_node_kind_is_refused_by_name() {
    let ron = r#"FlowSpec(vars: {}, entry: "x", nodes: { "x": (kind: "nope") })"#;
    let json = r#"{"vars": {}, "entry": "x", "nodes": { "x": { "kind": "nope" } }}"#;

    assert_eq!(unknown_kind(ron), Some(("x".to_owned(), "nope".to_owned())));

    // The same refusal, reached through either notation.
    for (notation, error) in ["ron", "json"].into_iter().zip(refusals(ron, json)) {
        assert!(
            matches!(error, Some(BotError::UnknownNodeKind { .. })),
            "{notation}: expected UnknownNodeKind, got {error:?}"
        );
    }
}

/// An oversized document is refused before a decoder walks it, in either
/// notation.
#[test]
fn an_oversized_document_is_refused() {
    let padding = " ".repeat(lgwks_bot::MAX_FLOW_BYTES);
    let oversized_ron = format!("{RON}{padding}");
    let oversized_json = format!("{JSON}{padding}");
    let limit = lgwks_bot::MAX_FLOW_BYTES;

    assert_eq!(
        too_large(&oversized_ron),
        Some((oversized_ron.len(), limit)),
        "the refusal did not name the size and the limit"
    );
    for (notation, error) in ["ron", "json"]
        .into_iter()
        .zip(refusals(&oversized_ron, &oversized_json))
    {
        assert!(
            matches!(error, Some(BotError::FlowTooLarge { .. })),
            "{notation}: expected FlowTooLarge, got {error:?}"
        );
    }
}

/// An unknown field is refused, so a typo is not silently ignored.
#[test]
fn an_unknown_field_is_refused() {
    let typo = r#"FlowSpec(vars: {}, entry: "end", nodes: { "end": (kind: "end") }, entyr: "x")"#;
    assert!(malformed(typo).is_some(), "an unknown field was accepted");
}

/// A refusal never carries a raw control character into a log line.
///
/// A document is untrusted input. Without escaping, a newline inside one would
/// forge a line in whatever log carries the refusal, and a reader could not
/// tell the forged line from a real one.
#[test]
fn a_refusal_escapes_control_characters() {
    let hostile = [
        "FlowSpec(vars: {},\u{7} entry: \"end\")",
        "FlowSpec(vars: {}, entry: \"\u{1b}[31mend\")",
        "FlowSpec(vars: {}, entry: \"end\", nodes: { \"end\": (kind: \"\u{0}nope\") })",
        "{\"vars\": {}, \"entry\": \"end\", \"nodes\": {\"end\": {\"kind\": \"en\\nd\"}}}",
    ];

    for source in hostile {
        for error in refusals(source, source) {
            if let Some(BotError::MalformedFlow { cause }) = error {
                assert!(
                    !cause.chars().any(char::is_control),
                    "a diagnostic carried a control character: {cause:?}"
                );
            }
        }
    }
}

/// The node kinds require a map, and this pins why.
///
/// `NodeKind` is serde internally tagged (`tag = "kind"`), and an internally
/// tagged enum needs its tag to be a real field, so RON's native
/// `End` / `Say(text: "hello")` spelling cannot decode. This test fails the day
/// that tagging changes, and that is the point: moving to external tagging
/// would make a flow read `Say(text: "hello")`, which is a deliberate change to
/// the wire contract rather than a silent one.
#[test]
fn the_node_kind_spelling_is_forced_by_internal_tagging() {
    let native = r#"FlowSpec(vars: {}, entry: "end", nodes: { "end": End })"#;
    let cause = malformed(native);
    assert!(
        cause
            .as_deref()
            .is_some_and(|cause| cause.contains("internally tagged")),
        "RON's native enum spelling should be refused for its tagging; got {cause:?}"
    );
}
