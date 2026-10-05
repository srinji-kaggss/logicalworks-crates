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
    nodes: { "end": end },
)"#;

/// The same document in JSON, where external tagging costs a unit variant its
/// name: the variant becomes the bare string it is named by.
const JSON: &str = r#"{
    "vars": {},
    "entry": "end",
    "nodes": { "end": "end" }
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
fn unknown_kind(decoded: Result<FlowSpec, BotError>) -> Option<(String, String)> {
    match decoded.err() {
        Some(BotError::UnknownNodeKind { node, kind }) => Some((node, kind)),
        _ => None,
    }
}

/// The `(bytes, limit)` an oversize refusal named, if that is what it was.
fn too_large(decoded: Result<FlowSpec, BotError>) -> Option<(usize, usize)> {
    match decoded.err() {
        Some(BotError::FlowTooLarge { bytes, limit }) => Some((bytes, limit)),
        _ => None,
    }
}

/// The diagnostic a malformed refusal carried, if that is what it was.
fn malformed(decoded: Result<FlowSpec, BotError>) -> Option<String> {
    match decoded.err() {
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
        "end": end,   // the only node
    },
)
"#;
    assert_eq!(FlowSpec::from_ron(annotated)?, FlowSpec::from_json(JSON)?);
    Ok(())
}

/// An unknown node kind is refused on either notation, and each names what it
/// can.
///
/// The two refusals differ, and the cause is the notation rather than the flow.
/// JSON checks a variant name against nothing, so serde's own error names
/// neither the variant nor the node, and the guard supplies both. RON checks
/// the name against the list it is handed and refuses before any visitor sees
/// it, so its error names the variant and the enum and cannot name the node.
/// Both refuse; only what each can say differs.
#[test]
fn an_unknown_node_kind_is_refused_by_name() {
    let ron = r#"FlowSpec(vars: {}, entry: "x", nodes: { "x": nope })"#;
    let json = r#"{"vars": {}, "entry": "x", "nodes": { "x": "nope" }}"#;

    assert_eq!(
        unknown_kind(FlowSpec::from_json(json)),
        Some(("x".to_owned(), "nope".to_owned())),
        "the JSON path should name the node and the kind it carried"
    );

    let cause = malformed(FlowSpec::from_ron(ron));
    assert!(
        cause.as_deref().is_some_and(|cause| cause.contains("nope")),
        "the RON path should name the variant it refused: {cause:?}"
    );

    // Neither notation accepts it, which is the property that matters.
    for (notation, error) in ["ron", "json"].into_iter().zip(refusals(ron, json)) {
        assert!(error.is_some(), "{notation}: an unknown kind was accepted");
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
        too_large(FlowSpec::from_ron(&oversized_ron)),
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
    assert!(
        malformed(FlowSpec::from_ron(typo)).is_some(),
        "an unknown field was accepted"
    );
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

/// A variant is written with its own name, and the tag spelling is gone.
///
/// This is the property external tagging exists for, so it is pinned in both
/// directions: the native form decodes, and the `(kind: "end")` map that the
/// internally tagged enums required no longer does. A change back to internal
/// tagging fails here rather than silently changing how every flow reads.
#[test]
fn a_variant_is_spelled_with_its_own_name() {
    // A unit variant is its own name; a variant with fields takes them in
    // parentheses, which is the shape RON is built for.
    let native = r#"FlowSpec(
        vars: {},
        entry: "greet",
        nodes: {
            "greet": say(text: "Hello"),
            "end": end,
        },
        edges: [ next(from: "greet", to: "end") ],
    )"#;
    assert!(
        FlowSpec::from_ron(native).is_ok(),
        "RON's native enum spelling should decode: {:?}",
        FlowSpec::from_ron(native)
    );

    let tagged = r#"FlowSpec(vars: {}, entry: "end", nodes: { "end": (kind: "end") })"#;
    assert!(
        malformed(FlowSpec::from_ron(tagged)).is_some(),
        "the internally tagged spelling should no longer decode"
    );
}

/// A unit variant becomes a bare string in JSON, and this is the visible cost
/// of the tagging choice on that side.
#[test]
fn json_pays_for_external_tagging_on_unit_variants() -> Result<(), Box<dyn std::error::Error>> {
    let spec = FlowSpec::from_json(JSON)?;
    let rendered = spec.to_json()?;
    assert!(
        rendered.contains("\"end\": \"end\""),
        "a unit variant should render as its own name; got:\n{rendered}"
    );
    Ok(())
}
