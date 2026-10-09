//! The map as JSON, for agents (#384).
//!
//! The document has the shape `lgwks_bot::script::Architecture::to_json` writes
//! for the `ARCHITECTURE` a script compiles to, field for field, so a tool's
//! reading and the compiled map compare as values rather than by eye.

use lgwks_std::json::{Map, Value};

use super::tree::{Script, StepShape};

impl Script {
    /// The architecture map as JSON: `{"flows": [{"name", "signature", "line",
    /// "steps"}]}`, each step `{"kind", "subject", "detail", "line", "steps"}`,
    /// the document `Architecture::to_json` writes for this script.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let flows = self
            .flows
            .iter()
            .map(|flow| {
                object([
                    ("name", Value::from(flow.shape.name.as_str())),
                    ("signature", Value::from(flow.shape.signature.as_str())),
                    ("line", Value::from(flow.shape.line)),
                    ("steps", step_values(&flow.shape.steps)),
                ])
            })
            .collect();
        object([("flows", Value::Array(flows))])
    }
}

/// `steps` and the steps nested in them, as JSON objects in source order.
fn step_values(steps: &[StepShape]) -> Value {
    Value::Array(
        steps
            .iter()
            .map(|step| {
                object([
                    // `StepKind` serializes by variant name, and the lexicon's
                    // `Kind` is spelled variant for variant the same: the macro
                    // emits `StepKind::<Kind's Debug>`, so this is that name.
                    ("kind", Value::from(format!("{:?}", step.kind))),
                    ("subject", Value::from(step.subject.as_str())),
                    ("detail", Value::from(step.detail.as_str())),
                    ("line", Value::from(step.line)),
                    ("steps", step_values(&step.children)),
                ])
            })
            .collect(),
    )
}

/// A JSON object from its fields, in the order given.
fn object<const FIELDS: usize>(fields: [(&str, Value); FIELDS]) -> Value {
    Value::Object(
        fields
            .into_iter()
            .map(|(name, value)| (name.to_owned(), value))
            .collect::<Map<String, Value>>(),
    )
}
