//! Seeded simulation of `script!` expansion: drawn scripts through the macro's
//! own path, `parse` then `emit`.
//!
//! One seed draws whole scripts — flows of `each`, `for`, `within`, `retry`,
//! `step` and `let x = <block>:` nested to depth three over plain lines — and
//! every family asserts a property of the Rust the macro writes for them,
//! against what the drawn script says:
//!
//! - every drawn script expands, and the expansion reads back as Rust tokens;
//! - each flow becomes one `async fn` carrying the forbidden lints before it,
//!   and the script emits one `ARCHITECTURE`;
//! - a drawn deadline or pause is written as its exact nanosecond count, and a
//!   drawn attempt count is admitted inside `1..=1000` and refused outside it;
//! - a refused line planted at any depth is refused with its replacement named;
//! - blank lines above a script change nothing in its expansion;
//! - repeated sibling blocks of one kind get distinct, numbered labels;
//! - the same seed expands to the same text and distinct seeds diverge.
//!
//! Sources: the script language's refusal table (`tests::REFUSALS`), SL-5 (every
//! refusal names its replacement), and the cost of a panicking or unbounded
//! line in unattended automation that the forbidden-lint set exists to close.

use std::fmt::Write as _;
use std::str::FromStr;

use lgwks_deps::proc_macro2::TokenStream;
use lgwks_std::seeded::{DrawError, Seeded};

use crate::tests::expand;

/// The seeds every family replays.
const SEEDS: [u64; 4] = [
    0x5C21_0000_0000_0001,
    0x5C21_0000_0000_0002,
    0x5C21_FFFF_0000_0003,
    0xE417_5C21_0000_0004,
];

/// How many scripts each family draws per seed.
const SCRIPTS: usize = 24;

/// What separates the words of an expansion: a recorded line sits against a
/// bracket as often as against a space (`new (2 ,`).
const SEPARATORS: [char; 7] = [' ', '(', ')', '[', ']', '{', '}'];

/// A family's result: a refused draw or a refused expansion, as text.
type Outcome = Result<(), String>;

/// Lines the language refuses, each with the fragment of the refusal that
/// names what to write instead.
const REFUSED: [(&str, &str); 8] = [
    ("let n = Option::unwrap(xs)", "unwrap()` ends the program"),
    ("panic!(\"no\")", "`panic!` ends the program"),
    ("loop { }", "a loop with no bound"),
    ("std::process::exit(1)", "ending the process from a flow"),
    (
        "let p = \"/etc/data.csv\"",
        "absolute path from one machine",
    ),
    ("tokio::spawn(async {})", "a spawned task has no owner"),
    ("let first = xs[0]", "indexing ends the program"),
    ("std::thread::sleep(pause)", "`thread::sleep` stalls"),
];

/// A drawn index in `0..bound`.
fn below(state: &mut Seeded, bound: usize) -> Result<usize, DrawError> {
    state.index(bound.max(1))
}

/// A drawn script under construction: its lines and the counters that keep
/// every binding name distinct.
struct Script {
    /// The source lines.
    lines: Vec<String>,
    /// The next binding number.
    next: usize,
    /// How many flows were written.
    flows: usize,
    /// The positions (line index, indent) of plain leaf lines a refusal may
    /// replace.
    leaves: Vec<(usize, usize)>,
}

impl Script {
    /// An empty script.
    fn new() -> Self {
        Self {
            lines: Vec::new(),
            next: 0,
            flows: 0,
            leaves: Vec::new(),
        }
    }

    /// Writes one line at `depth` (four spaces each).
    fn line(&mut self, depth: usize, text: &str) {
        self.lines.push(format!("{}{text}", "    ".repeat(depth)));
    }

    /// Writes a plain leaf line and records where it is.
    fn leaf(&mut self, depth: usize) {
        let number = self.next;
        self.next = self.next.saturating_add(1);
        self.leaves.push((self.lines.len(), depth));
        self.line(depth, &format!("let v{number} = {number}"));
    }

    /// The script as source text.
    fn text(&self) -> String {
        let mut text = self.lines.join("\n");
        text.push('\n');
        text
    }
}

/// A block header drawn from the language's block words.
fn header(state: &mut Seeded, script: &mut Script) -> Result<String, DrawError> {
    let number = script.next;
    script.next = script.next.saturating_add(1);
    Ok(match below(state, 6)? {
        0 => "each x in xs:".to_owned(),
        1 => "for x in xs:".to_owned(),
        2 => format!("within {}ms:", below(state, 5_000)?.saturating_add(1)),
        3 => format!(
            "retry up to {} times, waiting {}ms:",
            below(state, 1_000)?.saturating_add(1),
            below(state, 900)?.saturating_add(1)
        ),
        4 => format!("step s{number}:"),
        _ => format!("let b{number} = within 2s:"),
    })
}

/// Writes a drawn body of one to three lines at `depth`.
fn body(state: &mut Seeded, script: &mut Script, depth: usize) -> Result<(), DrawError> {
    let count = below(state, 3)?.saturating_add(1);
    for _ in 0..count {
        if depth < 4 && below(state, 3)? == 0 {
            let opened = header(state, script)?;
            script.line(depth, &opened);
            body(state, script, depth.saturating_add(1))?;
        } else {
            script.leaf(depth);
        }
    }
    Ok(())
}

/// A drawn script of one to three flows.
fn draw(state: &mut Seeded) -> Result<Script, DrawError> {
    let mut script = Script::new();
    let flows = below(state, 3)?.saturating_add(1);
    for index in 0..flows {
        let visibility = if below(state, 2)? == 0 { "pub " } else { "" };
        script.line(
            0,
            &format!("{visibility}flow f{index}(xs: Vec<u8>, pause: u64):"),
        );
        body(state, &mut script, 1)?;
        script.flows = script.flows.saturating_add(1);
    }
    Ok(script)
}

/// Every drawn script, with its seed, passed to `check`.
fn sweep(mut check: impl FnMut(u64, &mut Seeded, &Script) -> Outcome) -> Outcome {
    for seed in SEEDS {
        let mut state = Seeded::from_seed(seed);
        for _ in 0..SCRIPTS {
            let script = draw(&mut state).map_err(|error| error.to_string())?;
            check(seed, &mut state, &script)?;
        }
    }
    Ok(())
}

/// How many times `needle` occurs in `haystack`.
fn occurrences(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}

#[test]
/// Every drawn script expands, and its expansion reads back as Rust tokens.
fn sim_every_drawn_script_expands_to_rust() -> Outcome {
    sweep(|seed, _, script| {
        let expanded = expand(&script.text())
            .map_err(|refusal| format!("seed {seed:#x}: refused {refusal}:\n{}", script.text()))?;
        TokenStream::from_str(&expanded)
            .map(|_| ())
            .map_err(|error| format!("seed {seed:#x}: not Rust ({error})"))
    })
}

#[test]
/// Each flow is one `async fn` with the forbidden lints written before it, and
/// a script emits exactly one `ARCHITECTURE`.
fn sim_each_flow_is_one_guarded_async_fn() -> Outcome {
    sweep(|seed, _, script| {
        let expanded = expand(&script.text())?;
        assert_eq!(
            occurrences(&expanded, "async fn"),
            script.flows,
            "seed {seed:#x}: one async fn per flow"
        );
        assert_eq!(
            occurrences(&expanded, "forbid"),
            script.flows,
            "seed {seed:#x}: one forbid attribute per flow"
        );
        assert_eq!(
            occurrences(&expanded, "pub const ARCHITECTURE"),
            1,
            "seed {seed:#x}: one architecture map per script"
        );
        for index in 0..script.flows {
            let name = format!("async fn f{index} ");
            let at_fn = expanded
                .find(&name)
                .ok_or(format!("seed {seed:#x}: no {name}"))?;
            let guarded = expanded
                .get(..at_fn)
                .is_some_and(|before| before.contains("forbid"));
            assert!(guarded, "seed {seed:#x}: f{index} has its lints before it");
        }
        Ok(())
    })
}

#[test]
/// A drawn deadline is written as its exact nanosecond count.
fn sim_a_drawn_deadline_is_its_exact_nanosecond_count() -> Outcome {
    for seed in SEEDS {
        let mut state = Seeded::from_seed(seed);
        for _ in 0..SCRIPTS {
            let millis = below(&mut state, 100_000)
                .map_err(|error| error.to_string())?
                .saturating_add(1);
            let source = format!("flow f():\n    within {millis}ms:\n        1\n");
            let expanded = expand(&source)?;
            let nanos = u128::try_from(millis)
                .map_err(|error| error.to_string())?
                .saturating_mul(1_000_000);
            assert!(
                expanded.contains(&format!("from_nanos ({nanos})")),
                "seed {seed:#x}: {millis}ms is {nanos}ns in {expanded}"
            );
        }
    }
    Ok(())
}

#[test]
/// A drawn attempt count inside `1..=1000` is admitted; outside it, refused.
fn sim_attempt_counts_are_admitted_exactly_inside_their_range() -> Outcome {
    for seed in SEEDS {
        let mut state = Seeded::from_seed(seed);
        for _ in 0..SCRIPTS {
            let attempts = below(&mut state, 2_000).map_err(|error| error.to_string())?;
            let source = format!("flow f():\n    retry up to {attempts} times:\n        1\n");
            let admitted = (1..=1_000).contains(&attempts);
            match expand(&source) {
                Ok(_) => assert!(admitted, "seed {seed:#x}: {attempts} attempts expanded"),
                Err(refusal) => {
                    assert!(!admitted, "seed {seed:#x}: {attempts} refused: {refusal}");
                    assert!(
                        refusal.contains("outside 1..=1000"),
                        "seed {seed:#x}: the refusal names the range: {refusal}"
                    );
                }
            }
        }
    }
    Ok(())
}

#[test]
/// A refused line planted in place of any leaf, at any depth, is refused with
/// the fragment that names its replacement.
fn sim_a_planted_refusal_is_refused_at_any_depth() -> Outcome {
    sweep(|seed, state, script| {
        let pick = below(state, script.leaves.len()).map_err(|error| error.to_string())?;
        let &(at, depth) = script.leaves.get(pick).ok_or("a drawn script has a leaf")?;
        let which = below(state, REFUSED.len()).map_err(|error| error.to_string())?;
        let &(line, expected) = REFUSED.get(which).ok_or("the refusal table is not empty")?;
        let mut lines = script.lines.clone();
        let slot = lines.get_mut(at).ok_or("the leaf line exists")?;
        *slot = format!("{}{line}", "    ".repeat(depth));
        let mut source = lines.join("\n");
        source.push('\n');
        match expand(&source) {
            Ok(_) => Err(format!(
                "seed {seed:#x}: {line:?} at depth {depth} expanded"
            )),
            Err(refusal) => {
                assert!(
                    refusal.contains(expected),
                    "seed {seed:#x}: {line:?} refused as {refusal:?}, not {expected:?}"
                );
                Ok(())
            }
        }
    })
}

#[test]
/// Blank lines above a script move nothing but the source lines its steps'
/// `Site`s and its flows' shapes record, each by exactly the number of blank
/// lines: step keys, labels and every other token carry structure, never
/// position.
fn sim_blank_lines_move_only_the_recorded_source_lines() -> Outcome {
    sweep(|seed, state, script| {
        let blanks = below(state, 40).map_err(|error| error.to_string())?;
        let moved = format!("{}{}", "\n".repeat(blanks), script.text());
        let (here, there) = (expand(&script.text())?, expand(&moved)?);
        let words: Vec<&str> = here.split(SEPARATORS).collect();
        let moved_words: Vec<&str> = there.split(SEPARATORS).collect();
        assert_eq!(
            words.len(),
            moved_words.len(),
            "seed {seed:#x}: blank lines changed the expansion's shape"
        );
        let mut shifted = 0_usize;
        for (index, (word, moved_word)) in words.iter().zip(&moved_words).enumerate() {
            if word == moved_word {
                continue;
            }
            // A step's `Site` and a flow's shape record the line they start
            // on; nothing else may move.
            let context = || {
                words
                    .get(index.saturating_sub(8)..index.saturating_add(2).min(words.len()))
                    .map(|around| around.join(" "))
            };
            let line = word.parse::<usize>().map_err(|error| {
                format!(
                    "seed {seed:#x}: {word:?} moved and is no line ({error}): {:?}",
                    context()
                )
            })?;
            let moved_line = moved_word
                .parse::<usize>()
                .map_err(|error| error.to_string())?;
            assert_eq!(
                moved_line,
                line.saturating_add(blanks),
                "seed {seed:#x}: a recorded line moved by other than {blanks}"
            );
            shifted = shifted.saturating_add(1);
        }
        assert!(
            blanks == 0 || shifted > 0,
            "seed {seed:#x}: no Site recorded a source line at all"
        );
        Ok(())
    })
}

#[test]
/// `k` sibling blocks of one kind get the plain label and then `~2` to `~k`.
fn sim_repeated_siblings_get_distinct_numbered_labels() -> Outcome {
    for seed in SEEDS {
        let mut state = Seeded::from_seed(seed);
        for _ in 0..SCRIPTS {
            let count = below(&mut state, 6)
                .map_err(|error| error.to_string())?
                .saturating_add(2);
            let mut source = "flow f():\n".to_owned();
            for _ in 0..count {
                source.push_str("    retry up to 2 times:\n        1\n");
            }
            let expanded = expand(&source)?;
            assert!(
                expanded.contains("\"retry\""),
                "seed {seed:#x}: the first sibling keeps the plain label"
            );
            for ordinal in 2..=count {
                assert!(
                    expanded.contains(&format!("\"retry~{ordinal}\"")),
                    "seed {seed:#x}: sibling {ordinal} of {count} is numbered"
                );
            }
        }
    }
    Ok(())
}

/// The FNV-1a hash of every drawn script's expansion under `seed`.
fn expansion_trace(seed: u64) -> Result<u64, String> {
    let mut state = Seeded::from_seed(seed);
    let mut all = String::new();
    for _ in 0..SCRIPTS {
        let script = draw(&mut state).map_err(|error| error.to_string())?;
        let expanded = expand(&script.text())?;
        writeln!(all, "{expanded}").map_err(|error| error.to_string())?;
    }
    let mut trace = 0xcbf2_9ce4_8422_2325_u64;
    for byte in all.bytes() {
        trace = (trace ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    Ok(trace)
}

#[test]
/// The replay oracle: one seed, one expansion trace.
fn sim_the_same_seed_expands_to_the_same_trace() -> Outcome {
    for seed in SEEDS {
        assert_eq!(
            expansion_trace(seed)?,
            expansion_trace(seed)?,
            "seed {seed:#x}: the same seed must expand identically"
        );
    }
    Ok(())
}

#[test]
/// The divergence oracle: a sweep that ignored its seed would pass the replay.
fn sim_distinct_seeds_expand_differently() -> Outcome {
    assert_ne!(
        expansion_trace(SEEDS[0])?,
        expansion_trace(SEEDS[1])?,
        "two seeds drew the same scripts"
    );
    Ok(())
}
