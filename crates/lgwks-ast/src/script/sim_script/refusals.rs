//! The refusal families: a refused construct planted at any depth is refused at its own token, and still refused nested inside brackets.

use super::*;

// ── Refusals ──────────────────────────────────────────────────────────────

/// A construct the language refuses, as one line of a flow.
struct Plant {
    /// The line, without its indentation.
    line: &'static str,
    /// The text the refusal is spanned at: its first occurrence in `line`.
    at: &'static str,
    /// The part of the refusal that names what to write instead.
    says: &'static str,
}

/// How a planted value line starts; what follows it can be nested in brackets.
const LET: &str = "let planted = ";

/// Every defect class `refuse.rs` names, one line each.
static PLANTS: [Plant; 16] = [
    // Spaced as `unwrap ()` on purpose: the parser reads tokens, so the space
    // is invisible to it, and the plant proves whitespace cannot step around
    // the refusal the way it steps around a text search.
    Plant {
        line: "let planted = items.first().unwrap ()",
        at: "unwrap",
        says: "unwrap()` ends the program",
    },
    Plant {
        line: "let planted = Option::expect(items.first(), \"x\")",
        at: "expect",
        says: "`.expect()` ends the program",
    },
    Plant {
        line: "let planted = panic!(\"stop\")",
        at: "panic",
        says: "`panic!` ends the program",
    },
    Plant {
        line: "let planted = todo!()",
        at: "todo",
        says: "`todo!` ends the program",
    },
    Plant {
        line: "let planted = items[3]",
        at: "[",
        says: "indexing ends the program",
    },
    Plant {
        line: "let planted = site.load(\"/home/me/data\")",
        at: "\"/home",
        says: "an absolute path from one machine",
    },
    Plant {
        line: "let planted = loop { break 1 }",
        at: "loop",
        says: "a loop with no bound can run forever",
    },
    Plant {
        line: "let planted = while ready { }",
        at: "while",
        says: "a loop with no bound can run forever",
    },
    Plant {
        line: "let planted = std::thread::spawn(work)",
        at: "spawn",
        says: "a spawned task has no owner",
    },
    Plant {
        line: "let planted = std::thread::sleep(pause)",
        at: "sleep",
        says: "`thread::sleep` stalls every sibling",
    },
    Plant {
        line: "let planted = std::process::exit(1)",
        at: "exit",
        says: "ending the process from a flow",
    },
    Plant {
        line: "let planted = std::mem::forget(items)",
        at: "forget",
        says: "`mem::forget` leaks",
    },
    Plant {
        line: "let planted = unsafe { site.raw() }",
        at: "unsafe",
        says: "a script never needs `unsafe`",
    },
    Plant {
        line: "let planted = executor::block_on(work)",
        at: "block_on",
        says: "`block_on` inside a flow stalls",
    },
    Plant {
        line: "let planted = channel::unbounded_channel()",
        at: "unbounded_channel",
        says: "an unbounded channel grows",
    },
    Plant {
        line: "use std::thread::sleep",
        at: "thread",
        says: "`use` of `thread` inside a flow",
    },
];

/// Brackets a refused construct can be nested in; none of them indexes.
const WRAPPERS: [(&str, &str); 4] = [("outer(", ")"), ("{ ", " }"), ("vec![", "]"), ("(", ")")];

/// The line to plant and the column, from its first character, its refusal
/// must be spanned at: `plant` as written, or nested one to three brackets
/// deep when `nest` is set.
fn planted_line(draw: &mut Draw, plant: &Plant, nest: bool) -> Result<(String, usize), String> {
    let missing = || format!("`{}` does not contain `{}`", plant.line, plant.at);
    let Some(rest) = plant.line.strip_prefix(LET).filter(|_| nest) else {
        return Ok((
            plant.line.to_owned(),
            plant.line.find(plant.at).ok_or_else(missing)?,
        ));
    };
    let depth = draw.between(1, 3)?;
    let mut opening = String::new();
    let mut closing = Vec::with_capacity(depth);
    for _ in 0..depth {
        let &(open, close) = draw.pick(&WRAPPERS)?;
        opening.push_str(open);
        closing.push(close);
    }
    closing.reverse();
    let column = LET
        .len()
        .saturating_add(opening.len())
        .saturating_add(rest.find(plant.at).ok_or_else(missing)?);
    Ok((format!("{LET}{opening}{rest}{}", closing.concat()), column))
}

/// A refused construct planted at a drawn depth is refused at its own token,
/// with the replacement in the message — through the whole expansion and
/// through `refuse::check` on the line alone. Returns the trace and which
/// plants were drawn.
fn planting(first: u64, count: u64, nest: bool) -> Result<(u64, Vec<usize>), String> {
    let mut trace = Trace::default();
    let mut drawn = Vec::new();
    for offset in 0..count {
        let seed = SCRIPT_BASE.wrapping_add(first).wrapping_add(offset);
        let mut script = Script::drawn(seed)?;
        let points = script.openers();
        let under = *script.draw.pick(&points)?;
        let which = script.draw.index(PLANTS.len())?;
        let plant = PLANTS
            .get(which)
            .ok_or_else(|| format!("no plant {which}"))?;
        if !drawn.contains(&which) {
            drawn.push(which);
        }
        let (line, offset_in_line) = planted_line(&mut script.draw, plant, nest)?;
        let depth = script.insert_under(under, line.clone())?;
        let (source, starts) = script.render();
        let line_number = *starts
            .get(under.saturating_add(1))
            .ok_or_else(|| format!("seed {seed:#x}: the planted line has no start"))?;
        let column = depth.saturating_mul(INDENT).saturating_add(offset_in_line);
        let Err(error) = read(&source)? else {
            let refusal = format!("seed {seed:#x}: `{line}` was accepted:\n{source}");
            tracing::debug!(error = %refusal, "planting: returning an error to the caller");
            return Err(refusal);
        };
        spanned_at(seed, &error, (line_number, column), plant.says, &source)?;
        let alone: Vec<TokenTree> = stream(&line)?.into_iter().collect();
        let Err(direct) = refuse::check(&alone) else {
            let refusal = format!("seed {seed:#x}: refuse::check accepted `{line}` alone");
            tracing::debug!(error = %refusal, "planting: returning an error to the caller");
            return Err(refusal);
        };
        spanned_at(seed, &direct, (1, offset_in_line), plant.says, &line)?;
        trace.count(line_number);
        trace.count(column);
        trace.text(&error.to_string());
    }
    Ok((trace.0, drawn))
}

/// The refusal is spanned at `(line, column)` and names the replacement.
fn spanned_at(
    seed: u64,
    error: &Refusal,
    (line, column): (usize, usize),
    says: &str,
    source: &str,
) -> Result<(), String> {
    let start = error.span().start();
    let message = error.to_string();
    if (start.line, start.column) != (line, column) || !message.contains(says) {
        let refusal = format!(
            "seed {seed:#x}: expected `{says}` at {line}:{column}, the refusal was `{message}` \
             at {}:{}:\n{source}",
            start.line, start.column
        );
        tracing::debug!(error = %refusal, "spanned_at: returning an error to the caller");
        return Err(refusal);
    }
    Ok(())
}

/// [`planting`] at the planted line's own depth.
pub(super) fn plant_family(first: u64, count: u64) -> Result<u64, String> {
    planting(first, count, false).map(|(trace, _)| trace)
}

/// [`planting`] with the construct nested inside brackets.
pub(super) fn nested_plant_family(first: u64, count: u64) -> Result<u64, String> {
    planting(first, count, true).map(|(trace, _)| trace)
}

#[test]
fn sim_one_band_plants_every_refusal() -> Result<(), String> {
    let (_, drawn) = planting(0, SEEDS_PER_BAND, false)?;
    let missing: Vec<&str> = PLANTS
        .iter()
        .enumerate()
        .filter(|&(index, _)| !drawn.contains(&index))
        .map(|(_, plant)| plant.line)
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!("the band never planted {missing:?}"))
    }
}
