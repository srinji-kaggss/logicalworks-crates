//! Deterministic simulation of the register's `[policy]` declarations
//! (INV-DEP-16).
//!
//! The gate holds no repository's policy: which licences an inbound edge may
//! carry, the closed surface set and the freeze are each register's own
//! `[policy]` keys. One seed draws a whole block — which keys are present, their
//! members, the order the keys are written in and the spacing around each comma
//! — and an independent model says what the parse must answer: the block read
//! back member for member, or one typed refusal naming the key and the line it
//! was written on. Each family injects one fault class into the drawn block, so
//! a refusal reached for the wrong reason (another key, another line, another
//! variant) fails the family that asked about it, naming the seed.

use std::collections::BTreeSet;
use std::error::Error;

use lgwks_deps::contract::{Contract, ContractError};
use lgwks_deps::invariants::{ErrorKind, Register};

use crate::sim::{Rng, Trace, receipt};

type TestResult = Result<(), Box<dyn Error>>;

/// Seeds each family sweeps.
const SEEDS: u64 = 320;

/// Licence terms the generator draws, every one a valid accepted term.
const LICENCES: [&str; 10] = [
    "MIT",
    "MIT-0",
    "Apache-2.0",
    "Apache-2.0 WITH LLVM-exception",
    "BSD-3-Clause",
    "MPL-2.0",
    "Zlib",
    "0BSD",
    "ISC",
    "GPL-2.0+",
];

/// Surface names the generator draws, every one a valid identifier.
const SURFACES: [&str; 8] = [
    "lgwks_std",
    "lgwks_bot",
    "lgwks_deps",
    "lgwks_ast",
    "lgwks_macros",
    "alpha",
    "beta_2",
    "gamma-3",
];

/// The two tiers a freeze may name.
const TIERS: [&str; 2] = ["boundary", "vendor"];

/// The separators a list may be written with; the parse trims each member.
const SEPARATORS: [&str; 3] = [", ", ",", " , "];

/// The four declaration keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Key {
    Licences,
    Surfaces,
    Frozen,
    Tier,
}

impl Key {
    /// Every key, in declaration order.
    const ALL: [Self; 4] = [Self::Licences, Self::Surfaces, Self::Frozen, Self::Tier];

    /// The register spelling.
    const fn name(self) -> &'static str {
        match self {
            Self::Licences => "accepted_licenses",
            Self::Surfaces => "surfaces",
            Self::Frozen => "frozen_surfaces",
            Self::Tier => "frozen_tier",
        }
    }
}

/// One `[policy]` block as the generator drew it.
#[derive(Clone, Debug)]
struct Block {
    /// `accepted_licenses`, if declared.
    licences: Option<Vec<String>>,
    /// `surfaces`, if declared.
    surfaces: Option<Vec<String>>,
    /// `frozen_surfaces`, if declared.
    frozen: Option<Vec<String>>,
    /// `frozen_tier`, if declared.
    tier: Option<String>,
    /// The order the declared keys are written in.
    order: Vec<Key>,
    /// The separator each list is written with, as the draw left it.
    separator: Option<&'static str>,
}

impl Block {
    /// The list `key` names, if it is a list key and declared.
    fn list_mut(&mut self, key: Key) -> Option<&mut Vec<String>> {
        match key {
            Key::Licences => self.licences.as_mut(),
            Key::Surfaces => self.surfaces.as_mut(),
            Key::Frozen => self.frozen.as_mut(),
            Key::Tier => None,
        }
    }

    /// The list `key` names, if it is a list key and declared.
    fn list(&self, key: Key) -> Option<&[String]> {
        match key {
            Key::Licences => self.licences.as_deref(),
            Key::Surfaces => self.surfaces.as_deref(),
            Key::Frozen => self.frozen.as_deref(),
            Key::Tier => None,
        }
    }

    /// Whether `key` is declared.
    const fn declares(&self, key: Key) -> bool {
        match key {
            Key::Licences => self.licences.is_some(),
            Key::Surfaces => self.surfaces.is_some(),
            Key::Frozen => self.frozen.is_some(),
            Key::Tier => self.tier.is_some(),
        }
    }

    /// The written value of `key`.
    fn value(&self, key: Key) -> String {
        match (key, self.list(key), self.tier.as_deref()) {
            (Key::Tier, _, Some(tier)) => tier.to_owned(),
            (_, Some(list), _) => match self.separator {
                Some(separator) => list.join(separator),
                // No separator drawn means the members are written with nothing
                // between them, which the parse trims as one list. `SEPARATORS`
                // is a three-element constant, so this arm says what an emptied
                // table means rather than substituting one separator for it.
                None => list.concat(),
            },
            _ => String::new(),
        }
    }

    /// The register text, and the 1-based line each declared key is on.
    fn render(&self) -> (String, Vec<(Key, usize)>) {
        let mut text = String::from("[policy]\nenforce = true\n");
        let mut lines = Vec::with_capacity(self.order.len());
        // A key dropped from the block after the order was drawn is not written.
        let written = self.order.iter().filter(|&&key| self.declares(key));
        for (offset, &key) in written.enumerate() {
            text.push_str(key.name());
            text.push_str(" = \"");
            text.push_str(&self.value(key));
            text.push_str("\"\n");
            lines.push((key, offset.saturating_add(3)));
        }
        (text, lines)
    }
}

/// What the parse must answer for a block.
#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    /// Parsed; the accepted licences read back exactly.
    Parsed(Option<Vec<String>>),
    /// A list refused, naming the key and the 1-based line it is on.
    BadValue { key: String, line: usize },
    /// `frozen_tier` refused as a tier, on its line.
    BadTier { line: usize },
    /// One half of the freeze written without the other.
    Incomplete {
        present: &'static str,
        missing: &'static str,
    },
}

/// Up to `counts`'s drawn number of distinct members of `pool`, in a drawn
/// order.
fn draw_distinct(rng: &mut Rng, pool: &[&str], counts: &[usize]) -> Vec<String> {
    // No count to draw from means no distinct member is wanted, which is what
    // an empty `counts` table says; it is not a number of zero substituted for
    // one the seed chose.
    let Some(&want) = rng.pick(counts) else {
        return Vec::new();
    };
    let want = want.min(pool.len());
    let mut members: Vec<String> = Vec::with_capacity(want);
    while members.len() < want {
        let Some(&member) = rng.pick(pool) else {
            break;
        };
        if !members.contains(&member.to_owned()) {
            members.push(member.to_owned());
        }
    }
    members
}

/// The declared keys of `block`, in a drawn order.
fn shuffled_order(rng: &mut Rng, block: &Block) -> Vec<Key> {
    let mut order: Vec<Key> = Key::ALL
        .into_iter()
        .filter(|&key| block.declares(key))
        .collect();
    order.sort_by_cached_key(|_| rng.next_u64());
    order
}

/// A valid block: every declared list is clean and a freeze names surfaces the
/// block declares, whole.
fn draw_block(rng: &mut Rng) -> Block {
    let licences = rng
        .coin()
        .then(|| draw_distinct(rng, &LICENCES, &[1, 2, 3, 4, 5]));
    let surfaces = rng
        .coin()
        .then(|| draw_distinct(rng, &SURFACES, &[1, 2, 3, 4, 5, 6]));
    let (frozen, tier) = if rng.coin() {
        let pool: Vec<&str> = surfaces.as_ref().map_or_else(
            || SURFACES.to_vec(),
            |declared| declared.iter().map(String::as_str).collect(),
        );
        (
            Some(draw_distinct(rng, &pool, &[1, 2, 3])),
            rng.pick(&TIERS).map(|tier| (*tier).to_owned()),
        )
    } else {
        (None, None)
    };
    let mut block = Block {
        licences,
        surfaces,
        frozen,
        tier,
        order: Vec::new(),
        separator: rng.pick(&SEPARATORS).copied(),
    };
    block.order = shuffled_order(rng, &block);
    block
}

/// A clean block that declares `key`.
fn block_declaring(rng: &mut Rng, key: Key) -> Block {
    loop {
        let block = draw_block(rng);
        if block.declares(key) {
            return block;
        }
    }
}

/// The line `key` is written on, or a failure naming the seed.
fn line_of(seed: u64, lines: &[(Key, usize)], key: Key) -> Result<usize, Box<dyn Error>> {
    lines
        .iter()
        .find(|&&(written, _)| written == key)
        .map(|&(_, line)| line)
        .ok_or_else(|| format!("seed {seed:#x}: {} was not rendered", key.name()).into())
}

/// What the parse answered, in the model's terms.
fn observe(text: &str) -> Result<Verdict, Box<dyn Error>> {
    match Contract::parse(text) {
        Ok(contract) => Ok(Verdict::Parsed(
            contract.accepted_licenses().map(<[String]>::to_vec),
        )),
        Err(ContractError::BadPolicyValue { line, key, .. }) => Ok(Verdict::BadValue { key, line }),
        Err(ContractError::BadTier { line, .. }) => Ok(Verdict::BadTier { line }),
        Err(ContractError::IncompletePolicy { present, missing }) => {
            Ok(Verdict::Incomplete { present, missing })
        }
        Err(other) => Err(format!("a refusal the model has no arm for: {other}").into()),
    }
}

/// Checks one block against its expected verdict and returns its trace hash.
fn check(seed: u64, block: &Block, expected: &Verdict) -> Result<u64, Box<dyn Error>> {
    let (text, _) = block.render();
    let got = observe(&text)?;
    if &got != expected {
        let refusal: Result<u64, Box<dyn Error>> =
            Err(format!("seed {seed:#x}: {text:?} answered {got:?}, model {expected:?}").into());
        return refusal;
    }
    let mut trace = Trace::new();
    trace.record(&text);
    trace.record(&format!("{expected:?}"));
    Ok(receipt(&trace)?)
}

/// The seeds of `family`.
fn seeds(family: u64) -> impl Iterator<Item = u64> {
    (0..SEEDS).map(move |index| (family << 48) ^ index.wrapping_mul(0x9e37_79b9_7f4a_7c15))
}

// ── The model ───────────────────────────────────────────────────────────────

/// The model's licence term: an identifier, or `identifier WITH identifier`,
/// where an identifier is ASCII alphanumerics, `.`, `-` and `+`, and never one
/// of the expression operators.
fn is_term(term: &str) -> bool {
    let ident = |word: &str| {
        !word.is_empty()
            && !matches!(word, "AND" | "OR" | "WITH")
            && word
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '+'))
    };
    let words: Vec<&str> = term.split(' ').collect();
    match *words.as_slice() {
        [single] => ident(single),
        [licence, "WITH", exception] => ident(licence) && ident(exception),
        _ => false,
    }
}

/// The model's surface name: ASCII alphanumeric first, then alphanumerics,
/// `-` and `_`.
fn is_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphanumeric())
        && chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
}

/// Whether a written list is refused. A trailing empty member (a trailing
/// comma) is forgiven; an empty member anywhere else, a repeat, or a member
/// outside the vocabulary refuses the whole list.
fn list_is_bad(list: &[String], member: fn(&str) -> bool) -> bool {
    let mut members: Vec<&str> = list.iter().map(|item| item.trim()).collect();
    if members.len() > 1 && members.last().is_some_and(|last| last.is_empty()) {
        members.pop();
    }
    members.is_empty()
        || members.iter().enumerate().any(|(index, item)| {
            members.iter().take(index).any(|earlier| earlier == item) || !member(item)
        })
}

/// The model's verdict for a block whose lists may carry bad members and whose
/// freeze may be half-written or name an undeclared surface: keys are judged
/// in written order, then the freeze's completeness, then its membership.
fn model(seed: u64, block: &Block) -> Result<Verdict, Box<dyn Error>> {
    let (_, lines) = block.render();
    for &(key, line) in &lines {
        let bad = match key {
            Key::Licences => block
                .list(key)
                .is_some_and(|list| list_is_bad(list, is_term)),
            Key::Surfaces | Key::Frozen => block
                .list(key)
                .is_some_and(|list| list_is_bad(list, is_name)),
            Key::Tier => {
                if block
                    .tier
                    .as_deref()
                    .is_some_and(|tier| !TIERS.contains(&tier))
                {
                    return Ok(Verdict::BadTier { line });
                }
                false
            }
        };
        if bad {
            return Ok(Verdict::BadValue {
                key: key.name().to_owned(),
                line,
            });
        }
    }
    match (&block.frozen, &block.tier) {
        (&Some(_), &None) => {
            return Ok(Verdict::Incomplete {
                present: "frozen_surfaces",
                missing: "frozen_tier",
            });
        }
        (&None, &Some(_)) => {
            return Ok(Verdict::Incomplete {
                present: "frozen_tier",
                missing: "frozen_surfaces",
            });
        }
        _ => {}
    }
    if let (Some(frozen), Some(surfaces)) = (block.frozen.as_deref(), block.surfaces.as_deref())
        && frozen.iter().any(|name| !surfaces.contains(name))
    {
        return Ok(Verdict::BadValue {
            key: Key::Frozen.name().to_owned(),
            line: line_of(seed, &lines, Key::Frozen)?,
        });
    }
    Ok(Verdict::Parsed(block.licences.clone()))
}

/// Draws a block from `seed`, lets `fault` change it, and checks the parse
/// against the model; returns the trace hash.
fn run(seed: u64, fault: impl FnOnce(&mut Rng, &mut Block)) -> Result<u64, Box<dyn Error>> {
    let mut rng = Rng::new(seed);
    let mut block = draw_block(&mut rng);
    fault(&mut rng, &mut block);
    let expected = model(seed, &block)?;
    check(seed, &block, &expected)
}

/// Sweeps `family`'s seeds, each block drawn to declare `key`, under `fault`,
/// and returns how many seeds reached each verdict kind.
fn sweep(
    family: u64,
    key: Key,
    fault: impl Fn(&mut Rng, &mut Block),
) -> Result<BTreeSet<String>, Box<dyn Error>> {
    let mut kinds = BTreeSet::new();
    for seed in seeds(family) {
        let mut rng = Rng::new(seed);
        let mut block = block_declaring(&mut rng, key);
        fault(&mut rng, &mut block);
        let expected = model(seed, &block)?;
        check(seed, &block, &expected)?;
        kinds.insert(
            format!("{expected:?}")
                .split([' ', '('])
                .next()
                .map_or_else(String::new, str::to_owned),
        );
    }
    Ok(kinds)
}

// ── Families ────────────────────────────────────────────────────────────────

/// A clean block parses and reads its accepted licences back member for
/// member, in the order written; an undeclared set reads back as none.
#[test]
fn clean_blocks_round_trip() -> TestResult {
    let mut declared = 0_u64;
    for seed in seeds(1) {
        let mut rng = Rng::new(seed);
        let block = draw_block(&mut rng);
        declared = declared.saturating_add(u64::from(block.licences.is_some()));
        check(seed, &block, &Verdict::Parsed(block.licences.clone()))?;
    }
    assert!(
        (64..SEEDS.saturating_sub(64)).contains(&declared),
        "{declared} of {SEEDS} blocks declared a licence set; the draw must cover both"
    );
    Ok(())
}

/// A leading or interior empty member refuses its list, on its line.
#[test]
fn an_empty_member_is_refused_at_its_line() -> TestResult {
    for key in [Key::Licences, Key::Surfaces, Key::Frozen] {
        let kinds = sweep(2, key, |rng, block| {
            if let Some(list) = block.list_mut(key) {
                let at = if rng.coin() {
                    0
                } else {
                    list.len().saturating_sub(1)
                };
                list.insert(at, String::new());
            }
        })?;
        assert!(kinds.contains("BadValue"), "{key:?}: {kinds:?}");
    }
    Ok(())
}

/// A member written twice refuses its list, on its line.
#[test]
fn a_repeated_member_is_refused_at_its_line() -> TestResult {
    for key in [Key::Licences, Key::Surfaces, Key::Frozen] {
        let kinds = sweep(3, key, |rng, block| {
            if let Some(list) = block.list_mut(key)
                && let Some(repeated) = list.first().cloned()
            {
                let at = if rng.coin() { list.len() } else { 1 };
                list.insert(at, repeated);
            }
        })?;
        assert_eq!(kinds.len(), 1, "{key:?}: {kinds:?}");
        assert!(kinds.contains("BadValue"), "{key:?}: {kinds:?}");
    }
    Ok(())
}

/// A member outside its key's vocabulary — a licence expression with `OR` or
/// `AND`, a dangling `WITH`, a surface with a space, a leading separator or a
/// dot — refuses that list, on its line.
#[test]
fn an_out_of_vocabulary_member_is_refused_at_its_line() -> TestResult {
    const BAD_LICENCES: [&str; 5] = [
        "MIT OR Apache-2.0",
        "MIT AND Zlib",
        "MIT WITH",
        "WITH MIT",
        "MIT  ISC",
    ];
    const BAD_NAMES: [&str; 5] = ["lgwks std", "_lead", "-lead", "dot.ted", "ünïcode"];
    for key in [Key::Licences, Key::Surfaces, Key::Frozen] {
        let kinds = sweep(4, key, |rng, block| {
            let pool: &[&str] = if key == Key::Licences {
                &BAD_LICENCES
            } else {
                &BAD_NAMES
            };
            // Both tables are non-empty constants; the arm reports a fixture
            // that lost its bad member instead of pushing a value the seed
            // never drew into the block under test.
            let Some(&bad) = rng.pick(pool) else {
                return;
            };
            if let Some(list) = block.list_mut(key) {
                list.push(bad.to_owned());
            }
        })?;
        assert_eq!(kinds.len(), 1, "{key:?}: {kinds:?}");
        assert!(kinds.contains("BadValue"), "{key:?}: {kinds:?}");
    }
    Ok(())
}

/// One half of the freeze without the other is refused naming both halves.
#[test]
fn a_half_written_freeze_is_refused_naming_both_halves() -> TestResult {
    let kinds = sweep(5, Key::Frozen, |rng, block| {
        if rng.coin() {
            block.tier = None;
        } else {
            block.frozen = None;
        }
        block.order = shuffled_order(rng, block);
    })?;
    assert_eq!(kinds.len(), 1, "{kinds:?}");
    assert!(kinds.contains("Incomplete"), "{kinds:?}");
    Ok(())
}

/// A frozen surface the block's own surface set does not name is refused on
/// the `frozen_surfaces` line; with no surface set declared it is admitted.
#[test]
fn a_freeze_outside_the_declared_surfaces_is_refused() -> TestResult {
    let kinds = sweep(6, Key::Frozen, |_, block| {
        if let Some(frozen) = block.frozen.as_mut() {
            frozen.push("zeta_outside".to_owned());
        }
    })?;
    assert!(
        kinds.contains("BadValue") && kinds.contains("Parsed"),
        "both a declared and an undeclared surface set must be drawn: {kinds:?}"
    );
    Ok(())
}

/// A freeze tier outside `boundary` and `vendor` — another case, a tier the
/// gate answers elsewhere, a padded or empty spelling — is refused on its own
/// line.
#[test]
fn a_tier_outside_the_vocabulary_is_refused_on_its_line() -> TestResult {
    const BAD_TIERS: [&str; 6] = [
        "Boundary",
        "eliminate",
        "consolidate",
        "vendor ",
        "VENDOR",
        "",
    ];
    let kinds = sweep(7, Key::Tier, |rng, block| {
        // `BAD_TIERS` is a non-empty constant; a block with no drawn tier is a
        // block with no tier at all, which is a case this family does not test.
        if let Some(&tier) = rng.pick(&BAD_TIERS) {
            block.tier = Some(tier.to_owned());
        }
    })?;
    assert_eq!(kinds.len(), 1, "{kinds:?}");
    assert!(kinds.contains("BadTier"), "{kinds:?}");
    Ok(())
}

/// The order the keys are written in never changes a verdict, clean or not,
/// and a refusal follows its key to wherever that key was written.
#[test]
fn key_order_never_changes_the_verdict() -> TestResult {
    for seed in seeds(8) {
        let mut rng = Rng::new(seed);
        let mut block = draw_block(&mut rng);
        if rng.coin()
            && let Some(list) = block.licences.as_mut()
        {
            list.push("MIT OR ISC".to_owned());
        }
        if rng.coin()
            && let Some(list) = block.surfaces.as_mut()
        {
            list.push("bad name".to_owned());
        }
        for _ in 0..4 {
            block.order = shuffled_order(&mut rng, &block);
            let expected = model(seed, &block)?;
            check(seed, &block, &expected)?;
        }
    }
    Ok(())
}

/// The invariant register shares the dependency register's line reader but has
/// no meaning for these keys: the first one written is refused there by name,
/// on its line, and a block with none of them parses.
#[test]
fn the_invariant_register_refuses_every_policy_key() -> TestResult {
    for seed in seeds(9) {
        let block = draw_block(&mut Rng::new(seed));
        let (text, lines) = block.render();
        let first = lines.first().map(|&(key, line)| (key.name(), line));
        match (Register::parse(&text), first) {
            (Err(ErrorKind::UnknownKey { line, key }), Some((want_key, want_line))) => {
                assert_eq!(
                    (key.as_str(), line),
                    (want_key, want_line),
                    "seed {seed:#x}: {text:?}"
                );
            }
            (Ok(_), None) => {}
            (answer, first) => {
                let refusal: TestResult = Err(format!(
                    "seed {seed:#x}: {text:?} answered {:?}, expected the first key {first:?}",
                    answer.err()
                )
                .into());
                return refusal;
            }
        }
    }
    Ok(())
}

/// The same seed replays to the same trace, and distinct seeds diverge.
#[test]
fn the_same_seed_replays_and_distinct_seeds_diverge() -> TestResult {
    let mut traces = BTreeSet::new();
    for seed in seeds(10) {
        let fault = |rng: &mut Rng, block: &mut Block| {
            if rng.coin() {
                block.tier = None;
            }
        };
        let first = run(seed, fault)?;
        assert_eq!(
            first,
            run(seed, fault)?,
            "seed {seed:#x} replayed differently"
        );
        traces.insert(first);
    }
    assert!(
        traces.len() > 250,
        "{SEEDS} seeds drew only {} distinct traces",
        traces.len()
    );
    Ok(())
}
