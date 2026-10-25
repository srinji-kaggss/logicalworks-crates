//! The ALLOW-SILENCE generator: one lint attribute, written at one of six
//! placements in one of four spellings, at a level and over a lint list the seed
//! draws; and the two silences the scan can only name in its own words, a list
//! it cannot read and a file it never reads.
//!
//! It lives beside the model rather than inside it because the model had grown
//! past the size law with this family in it, and because the family is one
//! responsibility: which attribute, where, and what the rule must report for it.
//! `allow` and `expect` silence; `deny`, `warn` and `forbid` raise; a lint
//! outside clippy's silences exactly as a clippy lint does; a `cfg_attr`
//! wrapped attribute is the same act under a condition (#699); and so is the
//! same attribute spelled with raw identifiers or written inside a macro, which
//! rustc reads as the real one (#758).

use crate::sim::Rng;

use super::model::{
    Block, CLIPPY_LINTS, Drawn, Expect, Lints, OTHER_LINTS, Placement, SILENCING_LEVELS, Spelling,
};

/// File extensions an include may name; only `rs` is a file the walker reads.
const EXTENSIONS: [&str; 6] = ["rs", "inc", "txt", "in", "tpl", "rs.in"];

/// How a source file is pulled in. The first three and the last are reported when
/// the file is not a `.rs` file; the others are data or generated code, not
/// authored source, and are never reported.
const INCLUDES: [Include; 7] = [
    Include::Macro("include"),
    Include::Macro("std::include"),
    Include::Macro("core::include"),
    Include::Path,
    Include::Macro("include_str"),
    Include::Macro("include_bytes"),
    Include::Generated,
];

/// A way to pull a file into the build.
#[derive(Debug, Clone, Copy)]
enum Include {
    /// A macro, by path, called with the file name as one string literal.
    Macro(&'static str),
    /// A `#[path = ".."]` on a module declaration.
    Path,
    /// `include!(concat!(env!("OUT_DIR"), ..))`: generated code, not a file in
    /// the tree.
    Generated,
}

/// A lint attribute at `level` naming one to four lints, written at
/// `placement`, and one expected finding per lint when `level` silences.
pub(super) fn allow_block(
    rng: &mut Rng,
    index: u32,
    placement: Placement,
    level: &'static str,
    lints: Lints,
) -> Drawn<Block> {
    spelled_block(rng, index, placement, level, lints, false, Spelling::Plain)
}

/// The same attribute written inside a `cfg_attr`, which silences under a
/// condition and is therefore the same act (#699).
pub(super) fn cfg_attr_block(
    rng: &mut Rng,
    index: u32,
    placement: Placement,
    level: &'static str,
    lints: Lints,
) -> Drawn<Block> {
    spelled_block(rng, index, placement, level, lints, true, Spelling::Plain)
}

/// Writes `attribute` on an item at `placement`, and returns the offset of the
/// attribute's line. A macro placement puts the item inside the macro that makes
/// or returns it.
fn place(block: &mut Block, index: u32, placement: Placement, attribute: &str) -> usize {
    match placement {
        Placement::Item => {
            let at = block.line(attribute);
            block.line(format!("fn allowed_{index}() {{}}"));
            at
        }
        Placement::Statement => {
            block.line(format!("fn allowed_{index}() {{"));
            let at = block.line(format!("    {attribute}"));
            block.line("    let held = 1_u8;");
            block.line("    drop(held);");
            block.line("}");
            at
        }
        Placement::Method => {
            block.line(format!("impl Allowed{index} {{"));
            let at = block.line(format!("    {attribute}"));
            block.line("    fn method(&self) {}");
            block.line("}");
            at
        }
        Placement::TestFunction => {
            block.line("#[test]");
            let at = block.line(attribute);
            block.line(format!("fn allowed_{index}() {{}}"));
            at
        }
        Placement::MacroBody => {
            block.line(format!("macro_rules! make_{index} {{"));
            block.line("    ($($lint:ident),*) => {");
            let at = block.line(format!("        {attribute}"));
            block.line(format!("        fn allowed_{index}() {{}}"));
            block.line("    };");
            block.line("}");
            block.line(format!("make_{index}!();"));
            at
        }
        Placement::MacroArgument => {
            block.line(format!("macro_rules! keep_{index} {{"));
            block.line("    ($item:item) => { $item };");
            block.line("}");
            block.line(format!("keep_{index}! {{"));
            let at = block.line(format!("    {attribute}"));
            block.line(format!("    fn allowed_{index}() {{}}"));
            block.line("}");
            at
        }
    }
}

/// A lint attribute at `level` naming one to four lints, at `placement`, spelled
/// as `spelling` says and inside a `cfg_attr` when `nested`, and one expected
/// finding per lint when `level` silences.
///
/// A raw identifier names the same lint as the plain one, so the model expects
/// the plain name whatever the spelling; a path (`clippy::all`) has no raw form
/// and is always written plain.
pub(super) fn spelled_block(
    rng: &mut Rng,
    index: u32,
    placement: Placement,
    level: &'static str,
    lints: Lints,
    nested: bool,
    spelling: Spelling,
) -> Drawn<Block> {
    let raw_head = matches!(spelling, Spelling::RawHead | Spelling::RawBoth);
    let raw_lints = matches!(spelling, Spelling::RawLints | Spelling::RawBoth);
    let mut written = Vec::new();
    let mut silenced = Vec::new();
    for _ in 0..rng.between(1, 4) {
        let clippy = match lints {
            Lints::Mixed => rng.coin(),
            Lints::ClippyOnly => true,
            Lints::OtherOnly => false,
        };
        let table: &[&str] = if clippy { &CLIPPY_LINTS } else { &OTHER_LINTS };
        let name = *rng.pick_named("lints", table)?;
        written.push(if raw_lints && !name.contains("::") {
            format!("r#{name}")
        } else {
            name.to_owned()
        });
        if SILENCING_LEVELS.contains(&level) {
            silenced.push(name);
        }
    }
    let list = written.join(", ");
    let head = |name: &str| {
        if raw_head {
            format!("r#{name}")
        } else {
            name.to_owned()
        }
    };
    let attribute = if nested {
        format!("#[{}(any(), {}({list}))]", head("cfg_attr"), head(level))
    } else {
        format!("#[{}({list})]", head(level))
    };
    let mut block = Block::default();
    let at = place(&mut block, index, placement, &attribute);
    for name in silenced {
        block.want(
            at,
            Expect::Allow {
                level,
                lint: name.to_owned(),
                nested,
            },
        );
    }
    Ok(block)
}

/// A `macro_rules!` that silences the lints its caller names: the list in the
/// attribute is a metavariable repetition, which no reader can resolve to lints,
/// so the scan reports the silence with its lints shown as `..`.
pub(super) fn unreadable_block(rng: &mut Rng, index: u32) -> Drawn<Block> {
    let level = *rng.pick_named("levels", &["allow", "expect", "deny", "warn"])?;
    let nested = rng.coin();
    let attribute = if nested {
        format!("#[cfg_attr(any(), {level}($($lint),*))]")
    } else {
        format!("#[{level}($($lint),*)]")
    };
    let mut block = Block::default();
    let at = place(&mut block, index, Placement::MacroBody, &attribute);
    if SILENCING_LEVELS.contains(&level) {
        block.want(
            at,
            Expect::Said(if nested {
                format!("#[cfg_attr(.., {level}(..))]")
            } else {
                format!("#[{level}(..)]")
            }),
        );
    }
    Ok(block)
}

/// A source file pulled in by one of [`INCLUDES`], named with a drawn extension.
/// The scan reads `.rs` files and nothing else, so a name that does not end in
/// `.rs` is a file no scan ever sees, and reported as such; an include of data or
/// of generated code is not.
pub(super) fn include_block(rng: &mut Rng, index: u32) -> Drawn<Block> {
    let extension = *rng.pick_named("extensions", &EXTENSIONS)?;
    let how = *rng.pick_named("includes", &INCLUDES)?;
    let name = format!("fixture_{index}.{extension}");
    let mut block = Block::default();
    let (at, reported) = match how {
        Include::Macro(path) => {
            let at = block.line(format!("{path}!({name:?});"));
            (at, path.ends_with("include"))
        }
        Include::Path => {
            let at = block.line(format!("#[path = {name:?}]"));
            block.line(format!("mod hosted_{index};"));
            (at, true)
        }
        Include::Generated => {
            let at = block.line(format!(
                "include!(concat!(env!(\"OUT_DIR\"), \"/{name}\"));"
            ));
            (at, false)
        }
    };
    if reported && extension != "rs" {
        let call = match how {
            Include::Path => format!("#[path = {name:?}]"),
            _ => format!("include!({name:?})"),
        };
        block.want(
            at,
            Expect::Said(format!(
                "{call} — names a file the scan never reads (it reads .rs files only)"
            )),
        );
    }
    Ok(block)
}
