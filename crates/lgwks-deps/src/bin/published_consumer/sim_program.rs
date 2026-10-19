//! Simulated READMEs: one seed draws a whole rustdoc-style block or page, an
//! independent model of rustdoc's doctest rules says what program it makes, and
//! the real `main_rs` and `first_rust_block` run over it.
//!
//! Source: rustdoc's documentation-test rules (a hidden line is `# ` or a lone
//! `#`, `##` is a `#`, `#[..]` is code, `extern crate` and `#![..]` stay above
//! the wrapping `main`) and the four failures of the first run against the
//! published 3.0.0 / 4.0.0 / 2.0.0 releases (#526). The same seed must replay to
//! the same trace hash: the builder has no clock, hash order or thread in it.

use crate::program::{first_rust_block, main_rs};

/// Seeds each simulation sweeps.
const SEEDS: u64 = 2_000;

/// The head of the wrapping `main`, as rustdoc writes a doctest.
const HEAD: &str = "fn main() -> Result<(), Box<dyn std::error::Error>> {";

/// The FNV-1a offset basis, the start of every trace hash.
const BASIS: u64 = 0xCBF2_9CE4_8422_2325;

/// A line of page noise that never opens a Rust block.
const NOISE: [&str; 6] = [
    "text",
    "```text",
    "```rustscript",
    "```toml",
    "",
    "# heading",
];

/// Every line a drawn page may hold, fences of every kind among them.
const SOUP: [&str; 10] = [
    "```rust",
    "```rust,no_run",
    "```rustscript",
    "```text",
    "```",
    "text",
    "",
    "```toml",
    "# heading",
    "let a = 1;",
];

/// SplitMix64: a seeded stream with no clock and no global state.
struct Rng {
    /// The stream's position.
    state: u64,
}

impl Rng {
    /// A stream for `seed`.
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// The next 64 random bits.
    fn next(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mixed = (self.state ^ (self.state >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        let mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        mixed ^ (mixed >> 31)
    }

    /// Sixteen random bits: the low two bytes of the next draw.
    fn bits(&mut self) -> u16 {
        let [low, high, ..] = self.next().to_le_bytes();
        u16::from_le_bytes([low, high])
    }

    /// A draw in `0..bound`; an empty range is read as `0..1`.
    fn below(&mut self, bound: u16) -> u16 {
        self.bits().rem_euclid(bound.max(1))
    }

    /// An index in `0..bound`; an empty range is read as `0..1`.
    fn index(&mut self, bound: usize) -> usize {
        usize::from(self.bits()).rem_euclid(bound.max(1))
    }

    /// `count` lines drawn from `table`; an empty table draws empty lines.
    fn lines(&mut self, table: &[&'static str], count: u16) -> Vec<&'static str> {
        (0..count)
            .map(|_| match table.get(self.index(table.len())) {
                Some(line) => *line,
                None => "",
            })
            .collect()
    }
}

/// What a drawn line is, so the model can count the ones that must survive.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    /// An ordinary line.
    Plain,
    /// A line rustdoc hides: `# ` or a lone `#`.
    Hidden,
    /// `##[..]`, which rustdoc shows as `#[..]`.
    Escaped,
    /// `#[..]`, code that keeps its `#`.
    Attribute,
    /// The program's own `main`.
    Main,
}

/// One line of a drawn block: as written in the README and as the program shows it.
struct Piece {
    /// The README text.
    written: String,
    /// The text rustdoc compiles for it.
    shown: String,
    /// What it is.
    kind: Kind,
}

/// A piece whose shown text is its written text.
fn same(text: String, kind: Kind) -> Piece {
    Piece {
        shown: text.clone(),
        written: text,
        kind,
    }
}

/// One drawn line.
fn piece(rng: &mut Rng) -> Piece {
    let number = rng.below(1_000);
    let indent = String::from(if rng.below(3) == 0 { "    " } else { "" });
    match rng.below(10) {
        0 => Piece {
            written: format!("{indent}# use std::fmt::Write as w{number};"),
            shown: format!("{indent}use std::fmt::Write as w{number};"),
            kind: Kind::Hidden,
        },
        1 => Piece {
            written: format!("{indent}#"),
            shown: indent,
            kind: Kind::Hidden,
        },
        2 => Piece {
            written: format!("##[allow(unused_variables, dead_code{number})]"),
            shown: format!("#[allow(unused_variables, dead_code{number})]"),
            kind: Kind::Escaped,
        },
        3 => same(format!("#[derive(Debug, Clone{number})]"), Kind::Attribute),
        4 => same(format!("#![allow(dead_code{number})]"), Kind::Plain),
        5 => same(format!("extern crate dep{number};"), Kind::Plain),
        6 => Piece {
            written: format!("# extern crate hidden{number};"),
            shown: format!("extern crate hidden{number};"),
            kind: Kind::Hidden,
        },
        7 => same(
            format!("let done{number} = Ok::<(), std::io::Error>(());"),
            Kind::Plain,
        ),
        _ => same(
            format!("{indent}let value{number} = {number};"),
            Kind::Plain,
        ),
    }
}

/// A drawn block of one to twelve lines, and whether it declares its own `main`.
fn block(rng: &mut Rng) -> (Vec<Piece>, bool) {
    let count = rng.below(12).saturating_add(1);
    let mut pieces: Vec<Piece> = (0..count).map(|_| piece(rng)).collect();
    let own_main = rng.below(4) == 0;
    if own_main {
        let at = rng.index(pieces.len().saturating_add(1));
        pieces.insert(at, same("fn main() {}".to_owned(), Kind::Main));
    }
    (pieces, own_main)
}

/// The README text of `pieces`.
fn written(pieces: &[Piece]) -> String {
    let lines: Vec<&str> = pieces.iter().map(|each| each.written.as_str()).collect();
    lines.join("\n")
}

/// Whether the model puts `line` above the wrapping `main`.
fn crate_level(line: &str) -> bool {
    let body = line.trim_start();
    body.starts_with("#![") || body.starts_with("extern crate ")
}

/// The program the model says `pieces` make.
fn model(pieces: &[Piece], own_main: bool) -> String {
    let shown: Vec<&str> = pieces.iter().map(|each| each.shown.as_str()).collect();
    if own_main {
        return format!("{}\n", shown.join("\n"));
    }
    let (mut lines, body): (Vec<&str>, Vec<&str>) =
        shown.iter().copied().partition(|line| crate_level(line));
    let returns = body.iter().any(|line| line.contains("Ok::<"));
    lines.push(HEAD);
    lines.extend(body);
    if !returns {
        lines.push("Ok(())");
    }
    lines.push("}");
    format!("{}\n", lines.join("\n"))
}

/// FNV-1a over `text`, folded into `hash`.
fn fold(hash: u64, text: &str) -> u64 {
    text.bytes().fold(hash, |state, byte| {
        (state ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01B3)
    })
}

/// The trace hash of every program `SEEDS` seeds make.
fn trace() -> u64 {
    (0..SEEDS).fold(BASIS, |hash, seed| {
        let (pieces, _) = block(&mut Rng::new(seed));
        fold(hash, &main_rs(&written(&pieces)))
    })
}

#[test]
fn a_seeded_block_becomes_the_program_its_pieces_say() {
    for seed in 0..SEEDS {
        let (pieces, own_main) = block(&mut Rng::new(seed));
        let text = written(&pieces);
        assert_eq!(
            main_rs(&text),
            model(&pieces, own_main),
            "seed {seed} made a different program than rustdoc's rules say:\n{text}"
        );
    }
}

#[test]
fn the_same_seed_replays_to_the_same_trace_hash() {
    let first = trace();
    assert_eq!(
        first,
        trace(),
        "the builder has a clock, order or thread in it"
    );
    assert_ne!(first, BASIS, "no seed made any program");
}

#[test]
fn no_hiding_marker_survives_in_any_seeded_program() {
    for seed in 0..SEEDS {
        let (pieces, _) = block(&mut Rng::new(seed));
        let program = main_rs(&written(&pieces));
        for line in program.lines() {
            let body = line.trim_start();
            assert!(
                body != "#" && !body.starts_with("# "),
                "seed {seed} left a hidden line `{line}` in the program:\n{program}"
            );
        }
    }
}

#[test]
fn crate_level_lines_always_sit_above_the_wrapper() {
    for seed in 0..SEEDS {
        let (pieces, own_main) = block(&mut Rng::new(seed));
        if own_main {
            continue;
        }
        let program = main_rs(&written(&pieces));
        let lines: Vec<&str> = program.lines().collect();
        let head = lines.iter().position(|line| *line == HEAD);
        let last_crate = lines.iter().rposition(|line| crate_level(line));
        assert!(head.is_some(), "seed {seed} wrapped no main:\n{program}");
        assert!(
            last_crate.is_none_or(|at| Some(at) < head),
            "seed {seed} left a crate-level line below the wrapper:\n{program}"
        );
    }
}

#[test]
fn attributes_keep_their_hash_at_every_seed() {
    for seed in 0..SEEDS {
        let (pieces, _) = block(&mut Rng::new(seed));
        let kept = pieces
            .iter()
            .filter(|each| matches!(each.kind, Kind::Attribute | Kind::Escaped))
            .count();
        let program = main_rs(&written(&pieces));
        let found = program
            .lines()
            .filter(|line| line.trim_start().starts_with("#["))
            .count();
        assert_eq!(
            found, kept,
            "seed {seed}: every `#[..]` and `##[..]` is one code attribute:\n{program}"
        );
    }
}

#[test]
fn a_block_with_its_own_main_is_never_wrapped() {
    let mut seen = 0_u32;
    for seed in 0..SEEDS {
        let (pieces, own_main) = block(&mut Rng::new(seed));
        if !own_main {
            continue;
        }
        seen = seen.saturating_add(1);
        let program = main_rs(&written(&pieces));
        assert!(
            !program.contains(HEAD),
            "seed {seed} wrapped a block that has its own main:\n{program}"
        );
        assert_eq!(
            program.lines().count(),
            pieces.len(),
            "seed {seed} added or dropped a line of a block with its own main:\n{program}"
        );
        assert!(
            pieces.iter().any(|each| each.kind == Kind::Main),
            "seed {seed} drew no main"
        );
    }
    assert!(seen > 0, "no seed drew a block with its own main");
}

#[test]
fn a_returned_ok_means_no_second_one_and_none_means_exactly_one() {
    for seed in 0..SEEDS {
        let (pieces, own_main) = block(&mut Rng::new(seed));
        if own_main {
            continue;
        }
        let program = main_rs(&written(&pieces));
        let returns = pieces.iter().any(|each| each.shown.contains("Ok::<"));
        let closers = program.lines().filter(|line| *line == "Ok(())").count();
        assert_eq!(
            closers,
            usize::from(!returns),
            "seed {seed}: `Ok(())` ends a block unless it already returns its own:\n{program}"
        );
    }
}

/// The block the model says the first Rust fence of `lines` holds.
fn first_block(lines: &[&str]) -> Option<String> {
    let opens = |line: &&str| {
        line.strip_prefix("```rust")
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(',') || rest.starts_with(' '))
    };
    let start = lines.iter().position(opens)?;
    let after = || lines.iter().skip(start.saturating_add(1));
    let inside: Vec<&str> = after()
        .take_while(|line| !line.starts_with("```"))
        .copied()
        .collect();
    let closed = after().any(|line| line.starts_with("```"));
    if !closed || inside.iter().all(|line| line.trim().is_empty()) {
        return None;
    }
    Some(inside.join("\n"))
}

#[test]
fn page_soup_yields_the_first_closed_nonblank_rust_block_or_nothing() {
    for seed in 0..SEEDS {
        let mut rng = Rng::new(seed);
        let count = rng.below(20);
        let lines = rng.lines(&SOUP, count);
        let page = lines.join("\n");
        let found = first_rust_block(&page);
        assert_eq!(
            found,
            first_block(&lines),
            "seed {seed} read a different block than the fence rules say:\n{page}"
        );
        if let Some(text) = found {
            assert!(
                text.lines().all(|line| !line.starts_with("```")),
                "seed {seed} returned a block with a fence inside it:\n{page}"
            );
        }
    }
}

#[test]
fn a_known_block_is_found_whatever_noise_surrounds_it() {
    for seed in 0..SEEDS {
        let mut rng = Rng::new(seed);
        let wanted = format!("let kept{seed} = {seed};\nlet other = {};", rng.below(100));
        let before_count = rng.below(6);
        let before = rng.lines(&NOISE, before_count).join("\n");
        let after_count = rng.below(6);
        let after = rng.lines(&NOISE, after_count).join("\n");
        let page = format!("{before}\n```rust\n{wanted}\n```\n{after}\n");
        assert_eq!(
            first_rust_block(&page).as_deref(),
            Some(wanted.as_str()),
            "seed {seed} lost its block among noise:\n{page}"
        );
    }
}
