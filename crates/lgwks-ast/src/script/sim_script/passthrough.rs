//! The passthrough family (#389): the map of every drawn script lists as
//! passthrough exactly the lines the generator wrote as plain Rust.

use super::{SCRIPT_BASE, Script, Trace, accepted};

/// Whether a generated line says a word anywhere: `run`, `observe`, `act`,
/// `give back` or `fail`. The block words are headers, which `opens` marks.
fn says_a_word(text: &str) -> bool {
    text.split_whitespace()
        .any(|token| matches!(token, "run" | "observe" | "act" | "give" | "fail"))
}

/// A line with all whitespace removed: a group that runs onto the next lines
/// is written back with its line breaks, and the model wrote it with `\n`.
fn squeezed(text: &str) -> String {
    text.split_whitespace().collect()
}

/// The map lists as passthrough exactly the lines the generator wrote as
/// plain Rust, each at its source line and as written, and no line that says
/// a word, at any depth (#389).
pub(super) fn passthrough_family(first: u64, count: u64) -> Result<u64, String> {
    let mut trace = Trace::default();
    let mut seen = 0_usize;
    for offset in 0..count {
        let seed = SCRIPT_BASE.wrapping_add(first).wrapping_add(offset);
        let script = Script::drawn(seed)?;
        let (source, starts) = script.render();
        let expected: Vec<(usize, String)> = script
            .lines
            .iter()
            .zip(&starts)
            .filter(|&(src, _)| src.opens.is_none() && !says_a_word(&src.text))
            .map(|(src, &line)| (line, squeezed(&src.text)))
            .collect();
        let read: Vec<(usize, String)> = accepted(seed, &source)?
            .flows()
            .iter()
            .flat_map(|flow| flow.shape().passthrough())
            .map(|passed| (passed.line(), squeezed(passed.text())))
            .collect();
        if read != expected {
            let refusal = format!(
                "seed {seed:#x}: the generator wrote {expected:?} as plain Rust and the \
                 map lists {read:?}:\n{source}"
            );
            tracing::debug!(error = %refusal, "passthrough_family: returning an error to the caller");
            return Err(refusal);
        }
        seen = seen.saturating_add(read.len());
        for &(line, ref text) in &read {
            trace.count(line);
            trace.text(text);
        }
    }
    if seen == 0 {
        let refusal = "no script in the band wrote a plain Rust line".to_owned();
        tracing::debug!(error = %refusal, "passthrough_family: returning an error to the caller");
        return Err(refusal);
    }
    Ok(trace.0)
}
