//! The tautological-doc block generator: visibility, the doc spec, and the documented function it draws with the finding the model expects.

use super::*;

// ── tautological-doc blocks ─────────────────────────────────────────────────

/// Who can see a documented function.
#[derive(Debug, Clone, Copy)]
pub(super) enum Seen {
    /// `pub`.
    Public,
    /// `pub(crate)`.
    Crate,
    /// No visibility keyword.
    Private,
}

impl Seen {
    /// The keyword this visibility writes before `fn`.
    const fn keyword(self) -> &'static str {
        match self {
            Self::Public => "pub ",
            Self::Crate => "pub(crate) ",
            Self::Private => "",
        }
    }
}

/// Visibilities for a mixed draw, weighted toward the one the rule judges.
pub(super) const SEEN: [Seen; 5] = [
    Seen::Public,
    Seen::Public,
    Seen::Public,
    Seen::Crate,
    Seen::Private,
];

/// What a documented function looks like.
#[derive(Debug, Clone, Copy)]
pub(super) struct DocSpec {
    /// Who can see it.
    pub(super) seen: Seen,
    /// Whether it is a method in an inherent `impl`.
    pub(super) method: bool,
    /// Whether it carries `#[test]`.
    pub(super) test: bool,
    /// The fewest content tokens its doc carries.
    pub(super) fewest: u32,
    /// The most content tokens its doc carries.
    pub(super) most: u32,
    /// Whether every content token is a stem of the name.
    pub(super) paraphrase: bool,
    /// The most stop words mixed in.
    pub(super) stops: u32,
}

/// The doc spec every tautology family starts from: a public free function
/// whose doc carries one to six content tokens. The clean-file sweep starts
/// from it too, which is why it lives beside the type rather than the family.
pub(super) const PUBLIC_DOC: DocSpec = DocSpec {
    seen: Seen::Public,
    method: false,
    test: false,
    fewest: 1,
    most: 6,
    paraphrase: false,
    stops: 2,
};

/// A documented function shaped by `spec`, and the finding the model expects
/// for it.
pub(super) fn doc_block(rng: &mut Rng, index: u32, spec: DocSpec) -> Drawn<Block> {
    let first = *rng.pick_named("name words", &NAME_WORDS)?;
    let second = *rng.pick_named("name words", &NAME_WORDS)?;
    let name = format!("{first}_{second}_{index}");
    let total = rng.between(spec.fewest, spec.most);
    let shared = if spec.paraphrase {
        total
    } else {
        rng.below(total.saturating_add(1))
    };
    let mut words: Vec<String> = Vec::new();
    for word in 0..total {
        if word < shared {
            let stem = if rng.coin() { first } else { second };
            words.push(if rng.coin() {
                format!("{stem}s")
            } else {
                stem.to_owned()
            });
        } else {
            words.push((*rng.pick_named("other words", &OTHER_WORDS)?).to_owned());
        }
    }
    for _ in 0..rng.below(spec.stops.saturating_add(1)) {
        words.push((*rng.pick_named("stop words", &STOP_WORDS)?).to_owned());
    }
    shuffle(rng, &mut words)?;
    let mut block = Block::default();
    let indent = if spec.method {
        block.line(format!("impl DocHost{index} {{"));
        "    "
    } else {
        ""
    };
    if spec.test {
        block.line(format!("{indent}#[test]"));
    }
    if !words.is_empty() {
        let split = if rng.coin() {
            words.len().div_ceil(2)
        } else {
            words.len()
        };
        let (head, tail) = words
            .split_at_checked(split)
            .ok_or("a split inside the doc")?;
        block.line(format!("{indent}/// {}.", head.join(" ")));
        if !tail.is_empty() {
            block.line(format!("{indent}/// {}.", tail.join(" ")));
        }
    }
    let receiver = if spec.method { "&self" } else { "" };
    let at = block.line(format!(
        "{indent}{}fn {name}({receiver}) {{}}",
        spec.seen.keyword()
    ));
    if spec.method {
        block.line("}");
    }
    if matches!(spec.seen, Seen::Public)
        && !spec.test
        && (1..=6).contains(&total)
        && shared.saturating_mul(2) >= total
    {
        block.want(
            at,
            Expect::Doc {
                name,
                shared,
                total,
            },
        );
    }
    Ok(block)
}
