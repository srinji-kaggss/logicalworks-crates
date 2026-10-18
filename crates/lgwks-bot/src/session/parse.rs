//! Reading a flow from JSON or RON, with the size and node-kind guards. Split from `session.rs` (#526).

use super::{BTreeMap, BotError, Deserialize, MAX_FLOW_BYTES, serde};

/// Decode `text` as JSON, with the decoder's diagnostic as a string.
pub(super) fn parse_json<T>(text: &str) -> Result<T, String>
where
    T: serde::de::DeserializeOwned,
{
    crate::json::from_str(text).map_err(|error| error.to_string())
}

/// Decode `text` as RON, with the decoder's diagnostic as a string.
pub(super) fn parse_ron<T>(text: &str) -> Result<T, String>
where
    T: serde::de::DeserializeOwned,
{
    lgwks_std::ron::from_str(text).map_err(|error| error.to_string())
}

/// Refuse an oversized document before a decoder walks it.
///
/// Every notation is measured the same way, so a document cannot be too large
/// in one and acceptable in another.
pub(super) fn check_flow_size(source: &str) -> Result<(), BotError> {
    if source.len() > MAX_FLOW_BYTES {
        let refusal = Err(BotError::FlowTooLarge {
            bytes: source.len(),
            limit: MAX_FLOW_BYTES,
        });
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "check_flow_size: returning an error to the caller");
        return refusal;
    }
    Ok(())
}

/// A decoder failure, as a typed flow error.
///
/// The diagnostic is escaped before it is stored. A document is untrusted
/// input, and a raw newline or escape in one would otherwise forge lines in
/// whatever log line ends up carrying the refusal.
pub(super) fn malformed_flow(error: impl std::fmt::Display) -> BotError {
    BotError::MalformedFlow {
        cause: error.to_string().escape_debug().to_string(),
    }
}

/// Reject unknown tagged node kinds before serde turns them into a generic
/// malformed-document diagnostic.
pub(super) fn reject_unknown_node_kinds(
    declared: &BTreeMap<String, String>,
) -> Result<(), BotError> {
    for (node_id, kind) in declared {
        if !matches!(
            kind.as_str(),
            "say" | "ask" | "branch" | "handoff" | "refer" | "route" | "end"
        ) {
            let refusal = Err(BotError::UnknownNodeKind {
                node: node_id.clone(),
                kind: kind.clone(),
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "reject_unknown_node_kinds: returning an error to the caller");
            return refusal;
        }
    }
    Ok(())
}

/// The node kinds a document declares, keyed by node id.
///
/// Both notations decode into this, so the guard is written once rather than
/// once per notation. It exists because the guard has to read a variant's name
/// before the flow's own types have accepted it: by the time `FlowSpec` refuses
/// an unknown kind, serde has already discarded which node carried it.
///
/// The name is `FlowSpec` because RON enforces a struct's name: a document
/// written `FlowSpec(..)` is refused by a type that calls itself anything else,
/// so a guard named after itself would read no document at all and would report
/// silence, which is indistinguishable from finding nothing wrong. Every other
/// field of the document is ignored here, which is serde's default.
#[derive(Deserialize)]
#[serde(crate = "lgwks_std::json::serde", rename = "FlowSpec")]
pub(super) struct DeclaredNodes<const FIELDED: bool> {
    /// Every node's declared kind. Absent or empty is not this guard's
    /// business; the flow's own validation refuses an empty node map.
    #[serde(default)]
    pub(super) nodes: BTreeMap<String, VariantName<FIELDED>>,
}

/// A variant's name, read through serde's enum path.
///
/// The enum path is the only one that carries the name: RON writes a unit
/// variant as a bare identifier, and asking the decoder for `Any` hands back a
/// nameless unit, so the name reaches a reader only as a variant name.
///
/// `FIELDED` is which of serde's two variant accessors to read the content
/// with. A unit variant must be consumed as a unit and a variant carrying
/// anything must not, serde offers no way to ask which a given variant is, and
/// guessing wrong fails the decode rather than reading the name. So both are
/// attempted: this is the only place the flow's types are not the authority on
/// their own shape, and the cost is one extra decode of a document already
/// capped at [`MAX_FLOW_BYTES`].
pub(super) struct VariantName<const FIELDED: bool>(String);

impl<'de, const FIELDED: bool> Deserialize<'de> for VariantName<FIELDED> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        /// Reads a variant's name and discards whatever it carries.
        struct NameVisitor<const FIELDED: bool>;

        impl<'de, const FIELDED: bool> serde::de::Visitor<'de> for NameVisitor<FIELDED> {
            type Value = VariantName<FIELDED>;

            fn expecting(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                formatter.write_str("a node kind")
            }

            fn visit_enum<A>(self, data: A) -> Result<VariantName<FIELDED>, A::Error>
            where
                A: serde::de::EnumAccess<'de>,
            {
                use serde::de::VariantAccess;

                let (name, variant) = data.variant::<String>()?;
                if FIELDED {
                    variant.newtype_variant::<serde::de::IgnoredAny>()?;
                } else {
                    variant.unit_variant()?;
                }
                Ok(VariantName(name))
            }
        }

        deserializer.deserialize_enum("NodeKind", &[], NameVisitor::<FIELDED>)
    }
}

/// Merge what two shape-specific decodes of one document read.
///
/// A document holds unit variants and fielded ones at once, and serde reads a
/// variant's content through one accessor and not the other, so the guard
/// decodes the document once per shape and combines the results. A shape that
/// does not decode contributes nothing rather than failing the guard: a
/// document of only unit variants is exactly the case the fielded attempt
/// cannot read, and it is not an error. When neither shape reads, the guard has
/// no names to check and stays quiet, leaving the flow's own refusal to speak.
pub(super) fn merge_read_kinds<const FIRST: bool, const SECOND: bool>(
    first: Result<DeclaredNodes<FIRST>, String>,
    second: Result<DeclaredNodes<SECOND>, String>,
) -> BTreeMap<String, String> {
    let mut merged = BTreeMap::new();
    if let Ok(declared) = first {
        for (node_id, name) in declared.nodes {
            merged.insert(node_id, name.0);
        }
    }
    if let Ok(declared) = second {
        for (node_id, name) in declared.nodes {
            merged.insert(node_id, name.0);
        }
    }
    merged
}
