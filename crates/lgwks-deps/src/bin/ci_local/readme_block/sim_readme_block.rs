//! Seeded sweeps over the publisher block: one seed names one README, so a
//! failing seed is a complete reproduction and the same seed gives the same bytes.
//!
//! Each sweep draws 2,048 seeds from `lgwks_std::seeded::Seeded`, the estate's one
//! deterministic stream. The block is written out here in parts, with its own
//! spelling rather than the checker's constants, so a constant edited in
//! `readme_block` fails these sweeps instead of moving with them.

use std::fmt;
use std::num::TryFromIntError;

use lgwks_std::seeded::{DrawError, Seeded};

use super::{CRATES, WINDOW, front_page_gap, publisher_gap};

/// Why a sweep step could not run. A failed assertion panics as a test does; this
/// is for a step that had nothing to draw or could not be sized.
#[derive(Debug)]
enum SimError {
    /// The stream refused a draw.
    Draw(DrawError),
    /// A draw did not fit the width it was wanted at.
    Width(TryFromIntError),
    /// A table had no entry at the index drawn.
    Absent(&'static str),
    /// A damaged block passed the check.
    Unnoticed {
        /// The seed that named it.
        seed: u64,
        /// The crate whose README it was.
        name: &'static str,
    },
}

impl fmt::Display for SimError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Draw(ref error) => write!(formatter, "the stream refused a draw: {error}"),
            Self::Width(ref error) => write!(formatter, "a draw did not fit: {error}"),
            Self::Absent(what) => write!(formatter, "no entry for {what}"),
            Self::Unnoticed { seed, name } => {
                write!(
                    formatter,
                    "seed {seed}: the damage to {name} went unnoticed"
                )
            }
        }
    }
}

impl std::error::Error for SimError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match *self {
            Self::Draw(ref error) => Some(error),
            Self::Width(ref error) => Some(error),
            Self::Absent(_) | Self::Unnoticed { .. } => None,
        }
    }
}

impl From<DrawError> for SimError {
    fn from(error: DrawError) -> Self {
        Self::Draw(error)
    }
}

impl From<TryFromIntError> for SimError {
    fn from(error: TryFromIntError) -> Self {
        Self::Width(error)
    }
}

type TestResult = Result<(), SimError>;

/// How many seeds each sweep draws.
const SEEDS: u64 = 2048;

/// The ways `damage` can break a block.
const DAMAGES: usize = 11;

/// What a refusal says when the opening sentence is the broken piece.
const NO_SENTENCE: &str = "no sentence";

/// Lines a README holds around the block, relative links included.
const FILLER: [&str; 6] = [
    "# lgwks_x",
    "",
    "Everyday primitives with no async runtime required.",
    "```text",
    "| Crate | What it gives you |",
    "See [the guide](../../docs/guides/index.md) for the rest.",
];

/// The block as a README carries it, in parts one mutation can change at a time.
struct Parts {
    name: String,
    publisher: String,
    count: String,
    crates: Vec<String>,
    repository: String,
    guide: String,
}

impl Parts {
    /// The block of crate `name`, whole.
    fn whole(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            publisher: "Logical Works Incorporated publishes".to_owned(),
            count: "one of six".to_owned(),
            crates: CRATES.iter().map(|listed| (*listed).to_owned()).collect(),
            repository: "https://github.com/srinji-kaggss/logicalworks-crates#readme".to_owned(),
            guide: "https://github.com/srinji-kaggss/logicalworks-crates/blob/HEAD/docs/should-i-use.md"
                .to_owned(),
        }
    }

    /// The crate names as the sentence lists them: commas, then `and`.
    fn list(&self) -> String {
        let spelled: Vec<String> = self
            .crates
            .iter()
            .map(|listed| format!("`{listed}`"))
            .collect();
        match spelled.split_last() {
            Some((last, front)) if !front.is_empty() => {
                format!("{} and {last}", front.join(", "))
            }
            Some((last, _)) => last.clone(),
            None => String::new(),
        }
    }

    /// The opening words and the rest of the block.
    fn halves(&self) -> (String, String) {
        (
            format!("{} `{}` as {}", self.publisher, self.name, self.count),
            format!(
                "independently versioned crates in [logicalworks-crates]({}): {}. \
                 [Should I use logicalworks-crates?]({}) says when to reach for each one \
                 and when to skip it.",
                self.repository,
                self.list(),
                self.guide
            ),
        )
    }

    /// The block on one line.
    fn render(&self) -> String {
        let (opening, rest) = self.halves();
        format!("{opening} {rest}")
    }
}

/// A crate name the stream picks.
fn pick(stream: &mut Seeded) -> Result<&'static str, SimError> {
    let drawn = CRATES.get(stream.index(CRATES.len())?);
    Ok(*drawn.ok_or(SimError::Absent("a crate to pick"))?)
}

/// `block` with a space turned into a line break wherever the stream says, as an
/// author or a formatter rewraps a paragraph.
fn wrapped(block: &str, stream: &mut Seeded) -> Result<String, SimError> {
    let mut out = String::with_capacity(block.len());
    for ch in block.chars() {
        if ch == ' ' && stream.below(3)? == 0 {
            out.push_str("\n  ");
        } else {
            out.push(ch);
        }
    }
    Ok(out)
}

/// Up to seven filler lines the stream picks.
fn prose(stream: &mut Seeded) -> Result<String, SimError> {
    let mut out = String::new();
    for _ in 0..stream.below(8)? {
        let line = FILLER.get(stream.index(FILLER.len())?);
        out.push_str(line.ok_or(SimError::Absent("a filler line"))?);
        out.push('\n');
    }
    Ok(out)
}

/// `block`, rewrapped, between two runs of prose.
fn surrounded(block: &str, stream: &mut Seeded) -> Result<String, SimError> {
    let before = prose(stream)?;
    let middle = wrapped(block, stream)?;
    Ok(format!("{before}{middle}\n{}", prose(stream)?))
}

/// The README one seed names, and the crate it belongs to.
fn readme_of(seed: u64) -> Result<(String, &'static str), SimError> {
    let mut stream = Seeded::from_seed(seed);
    let name = pick(&mut stream)?;
    let block = Parts::whole(name).render();
    Ok((surrounded(&block, &mut stream)?, name))
}

/// Runs `each` on every seed of the sweep, stopping at the first failure.
fn sweep(each: impl Fn(u64) -> TestResult) -> TestResult {
    for seed in 0..SEEDS {
        each(seed)?;
    }
    Ok(())
}

/// Breaks one piece of `parts` and returns the words the refusal must hold.
fn damage(parts: &mut Parts, kind: usize) -> Result<String, SimError> {
    let named = match kind {
        0 => {
            parts.count = "one of seven".to_owned();
            None
        }
        1 => {
            parts.publisher = "Logical Works publishes".to_owned();
            None
        }
        2..=7 => {
            let gone = CRATES
                .get(kind.saturating_sub(2))
                .ok_or(SimError::Absent("a crate to drop"))?;
            parts.crates.retain(|listed| listed.as_str() != *gone);
            Some(format!("`{gone}`"))
        }
        8 => {
            parts.repository = "../../README.md".to_owned();
            Some("#readme)".to_owned())
        }
        9 => {
            parts.guide = "../../docs/should-i-use.md".to_owned();
            Some("should-i-use.md".to_owned())
        }
        _ => {
            parts.name = if parts.name == "lgwks_std" {
                "lgwks_bot".to_owned()
            } else {
                "lgwks_std".to_owned()
            };
            None
        }
    };
    Ok(match named {
        Some(words) => words,
        None => NO_SENTENCE.to_owned(),
    })
}

/// Source: crates.io serves a packaged README verbatim (Cargo's `readme` key), so
/// the sentence must survive any rewrapping an author or formatter applies (#719).
/// Proves: a whole block is accepted wherever it sits, for its own crate, and one
/// seed names one README (same seed, same bytes).
#[test]
fn sim_a_rewrapped_block_among_any_prose_is_accepted_and_replays() -> TestResult {
    sweep(|seed| {
        let (readme, name) = readme_of(seed)?;
        assert_eq!(publisher_gap(&readme, name), None, "seed {seed} for {name}");
        assert_eq!(readme_of(seed)?.0, readme, "seed {seed} must replay");
        Ok(())
    })?;
    assert_ne!(readme_of(0)?.0, readme_of(1)?.0, "distinct seeds diverge");
    Ok(())
}

/// Source: #719, which asks for who publishes the crate, that it is one of six and
/// a link to `docs/should-i-use.md`; cargo issue 8434 and crates.io issue 9939 for
/// the absolute links. Proves: each piece, removed or made relative, is refused with
/// the piece named, and a block for another crate is not this crate's.
#[test]
fn sim_damaging_any_one_piece_of_the_block_is_refused_and_named() -> TestResult {
    sweep(|seed| {
        let mut stream = Seeded::from_seed(seed);
        let name = pick(&mut stream)?;
        let mut parts = Parts::whole(name);
        let whole = surrounded(&parts.render(), &mut stream)?;
        assert_eq!(publisher_gap(&whole, name), None, "seed {seed}: control");
        let wanted = damage(&mut parts, stream.index(DAMAGES)?)?;
        let broken = surrounded(&parts.render(), &mut stream)?;
        let gap = publisher_gap(&broken, name).ok_or(SimError::Unnoticed { seed, name })?;
        assert!(
            gap.contains(&wanted),
            "seed {seed}: {gap:?} should hold {wanted:?}"
        );
        Ok(())
    })
}

/// Source: #719 (a block a reader can quote is one place, not five). Proves: the
/// window is real, so pieces scattered past it do not count and pieces inside it do,
/// however far into the README the block starts.
#[test]
fn sim_a_block_scattered_past_its_window_is_refused() -> TestResult {
    sweep(|seed| {
        let mut stream = Seeded::from_seed(seed);
        let name = pick(&mut stream)?;
        let (opening, rest) = Parts::whole(name).halves();
        let near = "x ".repeat(usize::try_from(stream.below(150)?)?);
        let far = "x ".repeat(usize::try_from(stream.below(150)?)?.saturating_add(WINDOW));
        let together = surrounded(&format!("{opening} {near}{rest}"), &mut stream)?;
        assert_eq!(publisher_gap(&together, name), None, "seed {seed}: near");
        let apart = surrounded(&format!("{opening} {far}{rest}"), &mut stream)?;
        assert!(publisher_gap(&apart, name).is_some(), "seed {seed}: far");
        Ok(())
    })
}

/// Source: #718 (one front-page line linking `llms.txt`) and llmstxt.org. Proves: a
/// markdown link to the file at the repository root passes however it is wrapped;
/// the bare name, a link into another directory and a link to another file do not.
#[test]
fn sim_the_front_page_needs_a_markdown_link_to_llms_txt() -> TestResult {
    let refused = [
        "Agent-readable index: [`llms.txt`](docs/llms.txt).",
        "Agent-readable index: `llms.txt`.",
        "Agent-readable index: [`llms.txt`](llms.txt.bak).",
        "Agent-readable index: [llms](https://example.com/llms.txt).",
    ];
    sweep(|seed| {
        let mut stream = Seeded::from_seed(seed);
        let line = "Agent-readable index: [`llms.txt`](llms.txt).";
        let page = surrounded(line, &mut stream)?;
        assert_eq!(front_page_gap(&page), None, "seed {seed}");
        let bad = refused
            .get(stream.index(refused.len())?)
            .ok_or(SimError::Absent("a refused line"))?;
        let page = surrounded(bad, &mut stream)?;
        assert!(front_page_gap(&page).is_some(), "seed {seed}: {bad}");
        Ok(())
    })
}
