//! The step-label family: a line inserted above a step changes none of the script's step labels.

use super::*;

// ── Step labels ───────────────────────────────────────────────────────────

/// Every label the tree gives a scope, depth first in source order: each
/// block's, and each `run` call's.
pub(super) fn labels(script: &tree::Script) -> Vec<String> {
    let mut out = Vec::new();
    for flow in script.flows() {
        block_labels(flow.body(), &mut out);
    }
    out
}

/// The labels under `block`.
fn block_labels(block: &Block, out: &mut Vec<String>) {
    for statement in block.statements() {
        match *statement {
            Statement::Rust(ref code)
            | Statement::Let(ref code)
            | Statement::GiveBack(ref code) => {
                code_labels(code, out);
            }
            Statement::Fail { ref reason, .. } => code_labels(reason, out),
            Statement::Run(ref call) => call_labels(call, out),
            Statement::Bind { ref block, .. } | Statement::Construct(ref block) => {
                construct_labels(block, out);
            }
            Statement::Together(ref together) => {
                for branch in together.branches() {
                    match *branch.value() {
                        BranchValue::Construct(ref construct) => construct_labels(construct, out),
                        BranchValue::Run(ref call) => call_labels(call, out),
                        BranchValue::Rust(ref code) => code_labels(code, out),
                    }
                }
            }
            Statement::For(ref each) => {
                out.push(each.label().to_owned());
                code_labels(each.items(), out);
                block_labels(each.body(), out);
            }
            Statement::If(ref chain) => {
                for branch in chain.branches() {
                    if let Some(condition) = branch.condition() {
                        code_labels(condition, out);
                    }
                    block_labels(branch.body(), out);
                }
            }
        }
    }
}

/// The labels of one block construct and everything under it.
fn construct_labels(construct: &Construct, out: &mut Vec<String>) {
    match *construct {
        Construct::Each(ref each) => {
            out.push(each.label().to_owned());
            code_labels(each.items(), out);
            block_labels(each.body(), out);
        }
        Construct::Within(ref within) => {
            out.push(within.label().to_owned());
            block_labels(within.body(), out);
        }
        Construct::Retry(ref again) => {
            out.push(again.label().to_owned());
            block_labels(again.body(), out);
        }
        Construct::Step(ref step) => {
            out.push(step.label().to_owned());
            block_labels(step.body(), out);
        }
    }
}

/// The labels of the `run` calls in a line of Rust.
fn code_labels(code: &Code, out: &mut Vec<String>) {
    for fragment in code.fragments() {
        match *fragment {
            Fragment::Token(_) => {}
            Fragment::Group { ref inner, .. } => code_labels(inner, out),
            Fragment::Run(ref call) => call_labels(call, out),
            Fragment::Domain(ref used) => {
                out.push(used.label().to_owned());
                code_labels(used.target(), out);
                if let Some(value) = used.value() {
                    code_labels(value, out);
                }
            }
        }
    }
}

/// A call's label, then the labels of the calls in its arguments.
fn call_labels(call: &Call, out: &mut Vec<String>) {
    out.push(call.label().to_owned());
    code_labels(call.arguments(), out);
}

/// A line inserted above a step changes none of the script's step labels.
pub(super) fn label_family(first: u64, count: u64) -> Result<u64, String> {
    let mut trace = Trace::default();
    let mut seen = 0_usize;
    for offset in 0..count {
        let seed = SCRIPT_BASE.wrapping_add(first).wrapping_add(offset);
        let mut script = Script::drawn(seed)?;
        let (source, _) = script.render();
        let before = labels(&accepted(seed, &source)?);
        let points = script.openers();
        let under = *script.draw.pick(&points)?;
        let name = script.name();
        script.insert_under(under, format!("let unrelated{name} = {name}"))?;
        let (edited, _) = script.render();
        let after = labels(&accepted(seed, &edited)?);
        if before != after {
            let refusal = format!(
                "seed {seed:#x}: an unrelated line moved the labels {before:?} to \
                 {after:?}:\n{edited}"
            );
            tracing::debug!(error = %refusal, "label_family: returning an error to the caller");
            return Err(refusal);
        }
        seen = seen.saturating_add(before.len());
        for label in &before {
            trace.text(label);
        }
    }
    if seen == 0 {
        let refusal =
            "no script in the band entered a labelled scope, so nothing was tested".to_owned();
        tracing::debug!(error = %refusal, "label_family: returning an error to the caller");
        return Err(refusal);
    }
    Ok(trace.0)
}
