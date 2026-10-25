//! Deterministic simulation of the manifest half of ALLOW-SILENCE: generated
//! `Cargo.toml` trees held to a model written from the rule's contract rather
//! than from the reader (#699).
//!
//! The model is four statements: a declaration inside a lint table
//! (`[lints]`, `[lints.*]`, `[workspace.lints]`, `[workspace.lints.*]`) or a
//! top-level `lints.*` key is the only thing that can silence; a level of
//! `allow` silences and every other level does not; a `scan-approved: <reason>`
//! comment on the declaration or in the comment block directly above it makes
//! that one declaration a reviewed exception, and a marker with no reason or one
//! separated by a blank line approves nothing; and a value the reader cannot
//! read is a finding rather than a pass.
//!
//! | Family | Property it pins |
//! |---|---|
//! | `manifest_band_00..03` | 1,024 seeded trees agree with the model, replay to one trace, and reach every outcome |
//! | `the_bands_cover_every_seed_exactly_once` | the bands partition the seed space |
//! | `an_unreadable_shape_is_always_a_finding` | no seed passes an inline table or an array |
//! | `an_approval_covers_one_declaration` | a justified marker is a reviewed exception, never a blanket one |
//! | `a_blank_line_breaks_the_approval_block` | the marker is read where it sits |
//! | `a_line_outside_a_lint_table_is_never_a_finding` | other tables are not lint tables |
//! | `carriage_returns_do_not_change_the_verdict` | the same manifest written two ways |
//! | `a_malformed_manifest_answers_without_aborting` | hostile lines are answered, not survived |
//! | `two_tenants_scan_their_own_manifests` | no state crosses between calls |

#![cfg(feature = "scan")]

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;

use lgwks_ast::scan::scan_manifest;

use crate::sim::{Rng, Trace, receipt};

/// The lines one generated manifest expects a finding on, and its evidence.
type Expected = Vec<(usize, String)>;

/// Seeds the bands sweep.
const SEEDS: u64 = 1_024;

/// How many bands the family is cut into, one declared test each.
const BANDS: u64 = 4;

/// The odd multiplier that spreads consecutive indices across the seed space.
const SPREAD: u64 = 0x9e37_79b9_7f4a_7c15;

/// Lints a generated declaration may name.
const LINTS: [&str; 6] = [
    "dead_code",
    "unused_variables",
    "unsafe_code",
    "missing_docs",
    "unused_must_use",
    "improper_ctypes",
];

/// Levels a generated declaration may carry. `allow` is the only one that
/// silences; `expect` is not a level cargo accepts in a manifest at all.
const LEVELS: [&str; 5] = ["allow", "warn", "deny", "forbid", "expect"];

/// Section headers a manifest may open, lint tables among them.
const TABLES: [&str; 4] = [
    "lints",
    "lints.rust",
    "lints.clippy",
    "workspace.lints.rust",
];

/// Reasons an approval may carry: every one is long enough to count.
const REASONS: [&str; 3] = [
    "the FFI modules own it",
    "the corpus needs this one",
    "a measured exception",
];

/// The marker a reviewed exception carries.
const MARKER: &str = "# scan-approved: ";

/// The words the reader uses for a value it cannot read.
const UNREADABLE: &str = "a lint declaration this reader cannot read";

/// Builds `line`'s finding for `scope`, as the reader renders it.
fn finding(scope: &str, line: &str) -> String {
    format!("{scope} {line}")
}

/// Builds the evidence of a declaration the reader cannot read.
fn unreadable(scope: &str, line: &str) -> String {
    format!("{scope} {line} — {UNREADABLE}")
}

/// The code part of a line, before any trailing comment.
fn code_of(line: &str) -> &str {
    match line.split_once(" #") {
        Some((code, _)) => code,
        None => line,
    }
}

/// One generated manifest: its text and the findings the model expects.
struct Manifest {
    /// The file's text.
    text: String,
    /// The findings, by line, in the reader's order.
    expected: Expected,
}

/// Appends a line, recording it as a finding when `evidence` is set.
fn push(manifest: &mut Manifest, text: &mut String, line: &str, evidence: Option<String>) {
    let number = text.lines().count().saturating_add(1);
    text.push_str(line);
    text.push('\n');
    if let Some(evidence) = evidence {
        manifest.expected.push((number, evidence));
    }
}

/// A lint table's declarations, with the approval and shape each draws.
fn lint_block(
    rng: &mut Rng,
    manifest: &mut Manifest,
    text: &mut String,
) -> Result<(), Box<dyn Error>> {
    let table = *rng.pick_named("lint tables", &["lints", "lints.rust", "lints.clippy"])?;
    let scope = format!("[{table}]");
    push(manifest, text, &format!("[{table}]"), None);
    for _ in 0..rng.between(1, 4) {
        let lint = *rng.pick_named("lints", &LINTS)?;
        let level = *rng.pick_named("levels", &LEVELS)?;
        let approved_above = match rng.below(4) {
            0 => {
                let reason = *rng.pick_named("reasons", &REASONS)?;
                push(manifest, text, &format!("{MARKER}{reason}"), None);
                true
            }
            1 => {
                // A marker with no reason approves nothing.
                push(manifest, text, MARKER.trim_end(), None);
                false
            }
            2 => {
                push(manifest, text, "# an ordinary comment", None);
                false
            }
            _ => false,
        };
        let (line, readable) = match rng.below(8) {
            0 => (format!("{lint} = {{ inner = \"allow\" }}"), false),
            1 => (format!("{lint} = [\"allow\"]"), false),
            2 => ("workspace = true".to_owned(), true),
            _ => (format!("{lint} = \"{level}\""), true),
        };
        let approved_here = rng.chance(120) && readable;
        let line = if approved_here {
            let reason = *rng.pick_named("reasons", &REASONS)?;
            format!("{line} {MARKER}{reason}")
        } else {
            line
        };
        let silenced = line.starts_with(&format!("{lint} = \"allow\""));
        let evidence = if !readable {
            Some(unreadable(&scope, code_of(&line)))
        } else if silenced && !approved_above && !approved_here {
            Some(finding(&scope, code_of(&line)))
        } else {
            None
        };
        push(manifest, text, &line, evidence);
    }
    Ok(())
}

/// A table that is not a lint table: nothing in it can silence.
fn other_block(
    rng: &mut Rng,
    manifest: &mut Manifest,
    text: &mut String,
) -> Result<(), Box<dyn Error>> {
    let table = *rng.pick_named("other tables", &["package", "dependencies", "features"])?;
    push(manifest, text, &format!("[{table}]"), None);
    for _ in 0..rng.between(1, 3) {
        let lint = *rng.pick_named("lints", &LINTS)?;
        push(manifest, text, &format!("{lint} = \"allow\""), None);
    }
    Ok(())
}

/// Top-level dotted lint keys, which are declarations without a table.
fn dotted_block(
    rng: &mut Rng,
    manifest: &mut Manifest,
    text: &mut String,
) -> Result<(), Box<dyn Error>> {
    let table = *rng.pick_named("lint tables", &TABLES)?;
    let lint = *rng.pick_named("lints", &LINTS)?;
    let level = *rng.pick_named("levels", &LEVELS)?;
    let line = format!("{table}.{lint} = \"{level}\"");
    let evidence = (level == "allow").then(|| finding(&format!("[{table}.{lint}]"), &line));
    push(manifest, text, &line, evidence);
    Ok(())
}

/// A table, dotted keys, or filler: the whole shape of a block.
///
/// A dotted key is only read as a lint declaration before the first section
/// header: after one, TOML makes it a member of that table, so the generator
/// writes it only at the top of a manifest and otherwise draws another table.
fn block(rng: &mut Rng, manifest: &mut Manifest, text: &mut String) -> Result<(), Box<dyn Error>> {
    let drawn = match rng.below(4) {
        0 => lint_block(rng, manifest, text),
        1 => other_block(rng, manifest, text),
        2 if text.is_empty() => dotted_block(rng, manifest, text),
        2 => lint_block(rng, manifest, text),
        _ => {
            push(manifest, text, "# filler", None);
            push(manifest, text, "[[bin]]", None);
            push(manifest, text, "name = \"x\"", None);
            Ok(())
        }
    };
    drawn?;
    // A blank line ends a comment block, so no approval survives into the next
    // block whatever it starts with.
    push(manifest, text, "", None);
    Ok(())
}

/// One seed's manifest.
fn generate(seed: u64) -> Result<Manifest, Box<dyn Error>> {
    let mut rng = Rng::new(seed);
    let mut manifest = Manifest {
        text: String::new(),
        expected: Vec::new(),
    };
    let mut text = String::new();
    for _ in 0..rng.between(2, 6) {
        block(&mut rng, &mut manifest, &mut text)?;
    }
    manifest.text = text;
    Ok(manifest)
}

/// The findings the reader reports, by line and evidence.
fn scanned(text: &str) -> Vec<(usize, String)> {
    scan_manifest(text)
        .iter()
        .map(|hit| (hit.line(), hit.snippet().to_owned()))
        .collect()
}

/// Scans one generated manifest and refuses any disagreement with the model.
fn agree(seed: u64, manifest: &Manifest) -> Result<Vec<(usize, String)>, Box<dyn Error>> {
    let observed = scanned(&manifest.text);
    if observed == manifest.expected {
        return Ok(observed);
    }
    Err(format!(
        "seed {seed:#x}: the reader reported {observed:#?}\nthe model expects {:#?}\nmanifest:\n{}",
        manifest.expected, manifest.text
    )
    .into())
}

/// The seed of the family's `index`-th scenario.
const fn seed_of(index: u64) -> u64 {
    0x5ca4_0000_0000_0000 ^ index.wrapping_mul(SPREAD)
}

/// The index range `band` sweeps.
fn band_seeds(band: u64) -> Result<std::ops::Range<u64>, Box<dyn Error>> {
    let per = SEEDS
        .checked_div(BANDS)
        .ok_or("the family is cut into a band")?;
    let first = band.saturating_mul(per);
    Ok(first..first.saturating_add(per))
}

/// Every scenario in `band`: each agrees with the model, replays to one trace,
/// and the band reaches each outcome.
fn manifest_band(band: u64) -> Result<(), Box<dyn Error>> {
    let mut approved = 0_usize;
    let mut silenced = 0_usize;
    let mut unread = 0_usize;
    for index in band_seeds(band)? {
        let seed = seed_of(index);
        let manifest = generate(seed)?;
        let found = agree(seed, &manifest)?;
        for finding in &found {
            if finding.1.contains(UNREADABLE) {
                unread = unread.saturating_add(1);
            } else {
                silenced = silenced.saturating_add(1);
            }
        }
        approved = approved.saturating_add(
            manifest
                .text
                .lines()
                .filter(|line| {
                    line.contains(MARKER) && line.trim().len() > MARKER.len().saturating_add(7)
                })
                .count(),
        );
        assert_eq!(
            receipt_of(seed)?,
            receipt_of(seed)?,
            "seed {seed:#x} replays to one trace"
        );
    }
    assert!(silenced > 0, "band {band} reached a silenced declaration");
    assert!(unread > 0, "band {band} reached an unreadable declaration");
    assert!(approved > 0, "band {band} reached a reviewed exception");
    Ok(())
}

/// One seed folded into a trace hash: the manifest and every finding.
fn receipt_of(seed: u64) -> Result<u64, Box<dyn Error>> {
    let manifest = generate(seed)?;
    let found = agree(seed, &manifest)?;
    let mut trace = Trace::new();
    trace.record(&manifest.text);
    for finding in &found {
        trace.record_number("ALLOW-SILENCE", finding.0);
        trace.record(&finding.1);
    }
    Ok(receipt(&trace)?)
}

macro_rules! bands {
    ($($name:ident => $band:expr);+ $(;)?) => {
        $(
            /// A seeded band of generated manifests, each agreeing with the model.
            #[test]
            fn $name() -> Result<(), Box<dyn Error>> {
                manifest_band($band)
            }
        )+
    };
}

bands!(
    manifest_band_00 => 0; manifest_band_01 => 1; manifest_band_02 => 2; manifest_band_03 => 3;
);

/// The bands partition the seed space: no gap, no overlap.
#[test]
fn the_bands_cover_every_seed_exactly_once() -> Result<(), Box<dyn Error>> {
    let mut covered = BTreeSet::new();
    for band in 0..BANDS {
        for index in band_seeds(band)? {
            assert!(covered.insert(index), "seed {index} is in two bands");
        }
    }
    assert_eq!(
        u64::try_from(covered.len())?,
        SEEDS,
        "the bands cover every seed"
    );
    Ok(())
}

/// No seed passes a value the reader cannot read: an inline table and an array
/// are findings on every manifest that writes one.
#[test]
fn an_unreadable_shape_is_always_a_finding() -> Result<(), Box<dyn Error>> {
    for index in 0..256_u64 {
        let seed = seed_of(index ^ 0x0bad);
        let manifest = generate(seed)?;
        let found = agree(seed, &manifest)?;
        let reported: BTreeSet<usize> = found.iter().map(|finding| finding.0).collect();
        for (number, line) in manifest.text.lines().enumerate() {
            let number = number.saturating_add(1);
            if line.contains("{ inner") || line.contains("[\"allow\"]") {
                assert!(
                    reported.contains(&number),
                    "seed {seed:#x}: a shape the reader cannot read was passed: {line}"
                );
            }
        }
    }
    Ok(())
}

/// A justified marker approves the declaration it sits above, and only that
/// one: the next declaration without its own marker is a finding.
#[test]
fn an_approval_covers_one_declaration() {
    let manifest = "[lints.rust]\n# scan-approved: the FFI modules own it\nunsafe_code = \"allow\"\ndead_code = \"allow\"\n";
    assert_eq!(
        scanned(manifest),
        vec![(4, "[lints.rust] dead_code = \"allow\"".to_owned())],
        "the second declaration carried no marker of its own"
    );
}

/// A blank line between the marker and the declaration breaks the block, so the
/// marker approves nothing.
#[test]
fn a_blank_line_breaks_the_approval_block() {
    let manifest =
        "[lints.rust]\n# scan-approved: the FFI modules own it\n\nunsafe_code = \"allow\"\n";
    assert_eq!(
        scanned(manifest),
        vec![(4, "[lints.rust] unsafe_code = \"allow\"".to_owned())]
    );
}

/// A lint-shaped declaration in a table that is not a lint table is not a lint
/// declaration.
#[test]
fn a_line_outside_a_lint_table_is_never_a_finding() {
    let manifest = "[dependencies]\ndead_code = \"allow\"\n[features]\nunsafe_code = \"allow\"\n";
    assert_eq!(scanned(manifest), Vec::new());
}

/// The same manifest written with carriage returns reports the same findings:
/// a line ending is not part of what the reader reads.
#[test]
fn carriage_returns_do_not_change_the_verdict() -> Result<(), Box<dyn Error>> {
    for index in 0..128_u64 {
        let seed = seed_of(index ^ 0x0d0d);
        let manifest = generate(seed)?;
        let unix = agree(seed, &manifest)?;
        let windows = manifest.text.replace('\n', "\r\n");
        assert_eq!(
            scanned(&windows),
            unix,
            "seed {seed:#x}: a carriage return changed the verdict"
        );
    }
    Ok(())
}

/// A manifest of hostile lines is answered line by line, never aborted on.
#[test]
fn a_malformed_manifest_answers_without_aborting() {
    let hostile = [
        "[lints.rust]\n= \"allow\"\n",
        "[lints.rust]\ndead_code\n",
        "[lints.rust]\ndead_code = \"\n",
        "[lints.rust]\n\"dead_code\" = \"allow\"\n",
        "[lints.\"rust\"]\ndead_code = \"allow\"\n",
        "[lints]\n[lints]\nlints = {}\n",
        "lints\n",
        "[",
        "]\n",
        "#\n#\n#\n",
        "[lints.rust]\r\nunsafe_code = \"allow\"\r\n",
        "\u{0}\u{1}\u{2}",
    ];
    for source in hostile {
        let found = scanned(source);
        assert!(
            found.len() <= source.lines().count(),
            "a manifest of {} lines answered {} findings: {found:?}",
            source.lines().count(),
            found.len()
        );
    }
}

/// Sixty-four manifests scanned on their own threads report exactly their own
/// findings: the reader keeps no state between calls.
#[test]
fn two_tenants_scan_their_own_manifests() -> Result<(), Box<dyn Error>> {
    let mut manifests = Vec::new();
    for index in 0..64_u64 {
        let seed = seed_of(index ^ 0x7ea4_0000);
        manifests.push((seed, generate(seed)?));
    }
    let failures = std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for entry in &manifests {
            let seed = entry.0;
            let manifest = &entry.1;
            handles.push(scope.spawn(move || -> Result<(), String> {
                for _ in 0..8 {
                    let observed = scanned(&manifest.text);
                    if observed != manifest.expected {
                        return Err(format!("seed {seed:#x}: {observed:#?}"));
                    }
                }
                Ok(())
            }));
        }
        let mut failures = Vec::new();
        for handle in handles {
            match handle.join() {
                Ok(Ok(())) => {}
                Ok(Err(failure)) => failures.push(failure),
                Err(_) => failures.push("a reader thread panicked".to_owned()),
            }
        }
        failures
    });
    assert!(failures.is_empty(), "tenants disagreed: {failures:#?}");
    Ok(())
}

/// The same seed replays its trace; one text never yields two answers, and
/// distinct seeds write distinct manifests.
#[test]
fn a_seed_replays_and_distinct_seeds_diverge() -> Result<(), Box<dyn Error>> {
    let mut by_text: BTreeMap<String, u64> = BTreeMap::new();
    for index in 0..64_u64 {
        let seed = seed_of(index ^ 0x5eed);
        let manifest = generate(seed)?;
        let first = receipt_of(seed)?;
        assert_eq!(
            first,
            receipt_of(seed)?,
            "seed {seed:#x} replayed differently"
        );
        if let Some(prior) = by_text.insert(manifest.text.clone(), first) {
            assert_eq!(prior, first, "seed {seed:#x}: one text, two receipts");
        }
    }
    assert!(
        by_text.len() >= 40,
        "64 seeds wrote only {} distinct manifests",
        by_text.len()
    );
    Ok(())
}

/// A marker with no reason to give is a rubber stamp: it must be
/// indistinguishable from an ordinary comment, so stripping the marker word from
/// every such line leaves the verdict exactly as it was.
#[test]
fn an_approval_with_no_reason_is_never_silence() -> Result<(), Box<dyn Error>> {
    let mut proved = 0_usize;
    let word = MARKER.trim_start_matches('#').trim();
    for index in 0..256_u64 {
        let seed = seed_of(index ^ 0xa990);
        let manifest = generate(seed)?;
        agree(seed, &manifest)?;
        let mut stripped = String::with_capacity(manifest.text.len());
        let mut changed = false;
        for (number, line) in manifest.text.lines().enumerate() {
            if number > 0 {
                stripped.push('\n');
            }
            let bare = line.trim().trim_start_matches('#').trim();
            if bare.is_empty()
                || !bare.starts_with(word)
                || bare.len() > word.len().saturating_add(7)
            {
                stripped.push_str(line);
                continue;
            }
            stripped.push_str("# an ordinary comment");
            changed = true;
        }
        if !changed {
            continue;
        }
        stripped.push('\n');
        proved = proved.saturating_add(1);
        assert_eq!(
            scanned(&stripped),
            scanned(&manifest.text),
            "seed {seed:#x}: a marker with no reason changed the verdict"
        );
    }
    assert!(proved > 0, "no seed wrote a marker without a reason");
    Ok(())
}

/// Every seeded manifest reports its findings once each, in line order: a
/// printer reads them without sorting and no line is named twice.
#[test]
fn every_seeded_manifest_reports_its_lines_in_order() -> Result<(), Box<dyn Error>> {
    for index in 0..256_u64 {
        let seed = seed_of(index ^ 0x0d0e);
        let manifest = generate(seed)?;
        let found = agree(seed, &manifest)?;
        assert!(
            found.windows(2).all(|pair| match *pair {
                [ref left, ref right] => left.0 < right.0,
                _ => false,
            }),
            "seed {seed:#x}: findings out of order or repeated: {found:#?}"
        );
    }
    Ok(())
}

/// A level that raises a lint rather than lowering it is never a finding, on
/// any seed: the rule is about silencing.
#[test]
fn a_raised_level_is_never_a_finding_on_any_seed() -> Result<(), Box<dyn Error>> {
    for index in 0..256_u64 {
        let seed = seed_of(index ^ 0x1e4e);
        let manifest = generate(seed)?;
        let reported: BTreeSet<usize> = agree(seed, &manifest)?
            .iter()
            .map(|finding| finding.0)
            .collect();
        for (number, line) in manifest.text.lines().enumerate() {
            let raised = ["= \"warn\"", "= \"deny\"", "= \"forbid\"", "= \"expect\""]
                .iter()
                .any(|level| line.contains(level));
            assert!(
                !(raised
                    && !line.contains(UNREADABLE)
                    && reported.contains(&number.saturating_add(1))),
                "seed {seed:#x}: a raised level was reported: {line}"
            );
        }
    }
    Ok(())
}
