//! The ALLOW-SILENCE generator: one lint attribute, written at one of four
//! placements, at a level and over a lint list the seed draws.
//!
//! It lives beside the model rather than inside it because the model had grown
//! past the size law with this family in it, and because the family is one
//! responsibility: which attribute, where, and what the rule must report for it.
//! `allow` and `expect` silence; `deny`, `warn` and `forbid` raise; a lint
//! outside clippy's silences exactly as a clippy lint does; and a `cfg_attr`
//! wrapped attribute is the same act under a condition (#699).

use crate::sim::Rng;

use super::model::{
    Block, CLIPPY_LINTS, Drawn, Expect, Lints, OTHER_LINTS, Placement, SILENCING_LEVELS,
};

/// A lint attribute at `level` naming one to four lints, written at
/// `placement`, and one expected finding per lint when `level` silences.
pub(super) fn allow_block(
    rng: &mut Rng,
    index: u32,
    placement: Placement,
    level: &'static str,
    lints: Lints,
) -> Drawn<Block> {
    silencing_block(rng, index, placement, level, lints, false)
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
    silencing_block(rng, index, placement, level, lints, true)
}

/// A lint attribute at `level` naming one to four lints, at `placement`, and one
/// expected finding per lint when `level` silences.
fn silencing_block(
    rng: &mut Rng,
    index: u32,
    placement: Placement,
    level: &'static str,
    lints: Lints,
    nested: bool,
) -> Drawn<Block> {
    let mut names = Vec::new();
    let mut silenced = Vec::new();
    for _ in 0..rng.between(1, 4) {
        let clippy = match lints {
            Lints::Mixed => rng.coin(),
            Lints::ClippyOnly => true,
            Lints::OtherOnly => false,
        };
        let table: &[&str] = if clippy { &CLIPPY_LINTS } else { &OTHER_LINTS };
        let name = *rng.pick_named("lints", table)?;
        names.push(name);
        if SILENCING_LEVELS.contains(&level) {
            silenced.push(name);
        }
    }
    let list = names.join(", ");
    let attribute = if nested {
        format!("#[cfg_attr(any(), {level}({list}))]")
    } else {
        format!("#[{level}({list})]")
    };
    let mut block = Block::default();
    let at = match placement {
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
    };
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
