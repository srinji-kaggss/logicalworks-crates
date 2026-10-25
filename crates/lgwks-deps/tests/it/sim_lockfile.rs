//! Seeded deterministic replay of the `Cargo.lock` reader (INV-LOCK-RESOLVED-TRUTH).
//!
//! One seed draws a whole resolved graph — names, versions, sources, checksums
//! — and renders it as Cargo writes it, with the noise a real lockfile carries:
//! the `@generated` banner, the top-level `version = 4`, `dependencies` arrays,
//! comments, blank lines, indentation and a trailing `[metadata]` or
//! `[[patch.unused]]` table. The reader's answer is checked against the graph
//! the seed drew, never against the reader itself.
//!
//! Sources: Cargo's lockfile format (`[[package]]` blocks of `key = "value"`
//! pairs, `source` absent for path and workspace packages, `checksum` the sha256
//! of the registry `.crate`), and the supply-chain incidents where an audit read
//! the declared graph rather than the resolved one. Requirements proved: every
//! resolved package is read with the fields its block declares and no others; a
//! block missing its name or version, or assigning an audited key twice, is
//! refused at its own line rather than skipped; noise never changes the graph; a
//! truncated lockfile never yields a package with a field no line declared.

use std::error::Error;

use lgwks_deps::lock::{LockError, Resolved, parse};

use crate::sim::{Rng, Trace, receipt};

/// The seeds every family replays, named so a failure names its seed.
const SEEDS: [u64; 4] = [
    0x10C0_0000_0000_0001,
    0x10C0_0000_0000_0002,
    0x10C0_FFFF_0000_0003,
    0xC0DE_10C0_0000_0004,
];

/// How many graphs each family draws per seed.
const GRAPHS: u32 = 24;

/// Package names a graph draws from: registry, workspace and hyphenated names.
const NAMES: [&str; 12] = [
    "serde",
    "serde_json",
    "tokio",
    "lgwks_std",
    "lgwks-bot",
    "blake3",
    "regex",
    "libc",
    "rustix",
    "memchr",
    "itoa",
    "ryu",
];

/// The sources a package may resolve from; `None` is a path or workspace package.
const SOURCES: [Option<&str>; 4] = [
    None,
    Some("registry+https://github.com/rust-lang/crates.io-index"),
    Some("sparse+https://index.crates.io/"),
    Some("git+https://github.com/example/dep?rev=0a1b2c#0a1b2c3d"),
];

/// The hex alphabet a checksum is drawn from.
const HEX: [char; 16] = [
    '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'a', 'b', 'c', 'd', 'e', 'f',
];

/// The keys the reader audits, each of which is refused when assigned twice.
const AUDITED: [&str; 4] = ["name", "version", "source", "checksum"];

/// A shared test result.
type TestResult = Result<(), Box<dyn Error>>;

/// One package of the drawn graph: the reference the reader is checked against.
#[derive(Debug, Clone)]
struct Package {
    /// The `name` key.
    name: &'static str,
    /// The `version` key.
    version: String,
    /// The `source` key, absent for a local package.
    source: Option<&'static str>,
    /// The `checksum` key, present only for some registry packages.
    checksum: Option<String>,
}

/// A defect planted in one block of an otherwise valid lockfile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fault {
    /// No defect.
    None,
    /// The block's `name` line is left out.
    OmitName(usize),
    /// The block's `version` line is left out.
    OmitVersion(usize),
    /// The block's `name` key is written quoted, which TOML reads as the same key.
    QuoteNameKey(usize),
    /// The block's `name` value is written bare, which the reader skips.
    BareNameValue(usize),
    /// One audited key of the block is assigned a second time.
    Duplicate(usize, &'static str),
}

/// Where each block and key landed in a rendered lockfile, one-based.
#[derive(Debug, Default)]
struct Layout {
    /// The line of each block's `[[package]]` header.
    headers: Vec<usize>,
    /// The audited keys each block declares, with the line each is on.
    keys: Vec<Vec<(&'static str, usize)>>,
    /// The line of the planted duplicate, when one was planted.
    duplicate: Option<usize>,
}

impl Layout {
    /// The header line of `block`, or a named fixture defect.
    fn header(&self, block: usize) -> Result<usize, Box<dyn Error>> {
        Ok(self
            .headers
            .get(block)
            .copied()
            .ok_or("the rendered lockfile has no header for that block")?)
    }

    /// Whether `block` declares `key` on a line at or before `cut`.
    fn declares(&self, block: usize, key: &str, cut: usize) -> bool {
        self.keys
            .get(block)
            .is_some_and(|keys| keys.iter().any(|entry| entry.0 == key && entry.1 <= cut))
    }
}

/// A rendered lockfile: its lines and where everything in it sits.
struct Rendered {
    /// The lines, without terminators.
    lines: Vec<String>,
    /// Where each block and key landed.
    layout: Layout,
}

impl Rendered {
    /// The lockfile joined with `terminator`.
    fn text(&self, terminator: &str) -> String {
        self.lines.join(terminator)
    }

    /// Appends one line and returns its one-based number.
    fn push(&mut self, line: String) -> usize {
        self.lines.push(line);
        self.lines.len()
    }
}

/// One drawn package.
fn draw_package(rng: &mut Rng) -> Result<Package, Box<dyn Error>> {
    let name = *rng.pick_named("names", &NAMES)?;
    let version = format!("{}.{}.{}", rng.below(5), rng.below(40), rng.below(300));
    let source = *rng.pick_named("sources", &SOURCES)?;
    let registry = source.is_some_and(|text| !text.starts_with("git+"));
    let checksum = if registry && rng.coin() {
        let mut digest = String::with_capacity(64);
        for _ in 0..64 {
            digest.push(*rng.pick_named("hex", &HEX)?);
        }
        Some(digest)
    } else {
        None
    };
    Ok(Package {
        name,
        version,
        source,
        checksum,
    })
}

/// A drawn graph of `low..=high` packages.
fn draw_graph(rng: &mut Rng, low: u32, high: u32) -> Result<Vec<Package>, Box<dyn Error>> {
    (0..rng.between(low, high))
        .map(|_| draw_package(rng))
        .collect()
}

/// Indentation and spacing noise around one line, when `noisy`.
fn dress(rng: &mut Rng, noisy: bool, line: &str) -> String {
    if !noisy {
        return line.to_owned();
    }
    let lead = ["", " ", "  ", "\t"];
    let trail = ["", " ", "\t"];
    match (rng.pick(&lead), rng.pick(&trail)) {
        (Some(before), Some(after)) => format!("{before}{line}{after}"),
        _ => line.to_owned(),
    }
}

/// Lines the reader must ignore inside a block, drawn when `noisy`.
///
/// `array_open` says the block has not yet been given a `dependencies` array.
/// A key defined twice in one table is invalid TOML (1.0.0, "Keys"), and the
/// reader refuses it as it refuses a second `name`, so the array is written at
/// most once per block.
fn block_noise(rng: &mut Rng, noisy: bool, array_open: &mut bool, out: &mut Rendered) {
    if !noisy {
        return;
    }
    match rng.below(5) {
        0 => {
            out.push(String::new());
        }
        1 => {
            out.push("# a comment Cargo never writes".to_owned());
        }
        2 if *array_open => {
            *array_open = false;
            out.push("dependencies = [".to_owned());
            out.push(" \"serde\",".to_owned());
            out.push(" \"libc 0.2.170\",".to_owned());
            out.push("]".to_owned());
        }
        3 => {
            out.push("replace = \"serde 1.0.0\"".to_owned());
        }
        _ => {}
    }
}

/// The audited key lines of one block, before any fault is applied.
fn key_lines(package: &Package) -> Vec<(&'static str, String)> {
    let mut keys = vec![
        ("name", format!("name = \"{}\"", package.name)),
        ("version", format!("version = \"{}\"", package.version)),
    ];
    if let Some(source) = package.source {
        keys.push(("source", format!("source = \"{source}\"")));
    }
    if let Some(ref checksum) = package.checksum {
        keys.push(("checksum", format!("checksum = \"{checksum}\"")));
    }
    keys
}

/// One key line as `fault` rewrites it: `None` to leave it out, and whether the
/// reader will still see it as the key.
fn faulted_line(
    fault: Fault,
    index: usize,
    key: &str,
    line: String,
    name: &str,
) -> Option<(String, bool)> {
    match fault {
        Fault::OmitName(at) if at == index && key == "name" => None,
        Fault::OmitVersion(at) if at == index && key == "version" => None,
        Fault::QuoteNameKey(at) if at == index && key == "name" => {
            Some((format!("\"name\" = \"{name}\""), true))
        }
        Fault::BareNameValue(at) if at == index && key == "name" => {
            Some((format!("name = {name}"), false))
        }
        _ => Some((line, true)),
    }
}

/// Renders one block, applying `fault` when it names this block.
fn render_block(
    rng: &mut Rng,
    noisy: bool,
    index: usize,
    package: &Package,
    fault: Fault,
    out: &mut Rendered,
) {
    let header = out.push("[[package]]".to_owned());
    out.layout.headers.push(header);
    let mut declared = Vec::new();
    let mut array_open = true;
    let mut keys = key_lines(package);
    if noisy && keys.len() > 1 && rng.coin() {
        keys.swap(0, 1);
    }
    for (key, line) in keys {
        block_noise(rng, noisy, &mut array_open, out);
        let Some((written, seen)) = faulted_line(fault, index, key, line, package.name) else {
            continue;
        };
        let number = out.push(dress(rng, noisy, &written));
        if seen {
            declared.push((key, number));
        }
    }
    if let Fault::Duplicate(at, key) = fault
        && at == index
    {
        let number = out.push(format!("{key} = \"again\""));
        out.layout.duplicate = Some(number);
    }
    block_noise(rng, noisy, &mut array_open, out);
    out.layout.keys.push(declared);
}

/// Renders `graph` as a lockfile, with noise when `noisy` and `fault` planted.
fn render(rng: &mut Rng, graph: &[Package], noisy: bool, fault: Fault) -> Rendered {
    let mut out = Rendered {
        lines: Vec::new(),
        layout: Layout::default(),
    };
    out.push("# This file is automatically @generated by Cargo.".to_owned());
    out.push("# It is not intended for manual editing.".to_owned());
    out.push("version = 4".to_owned());
    for (index, package) in graph.iter().enumerate() {
        out.push(String::new());
        render_block(rng, noisy, index, package, fault, &mut out);
    }
    if noisy {
        match rng.below(3) {
            0 => {
                out.push("[metadata]".to_owned());
                out.push("\"checksum serde 1.0.0 (registry+x)\" = \"abc\"".to_owned());
            }
            1 => {
                out.push("[[patch.unused]]".to_owned());
                out.push("name = \"unused\"".to_owned());
                out.push("version = \"9.9.9\"".to_owned());
            }
            _ => {}
        }
    }
    out
}

/// Asserts the reader's packages are exactly the drawn graph, in order.
fn assert_graph(seed: u64, read: &[Resolved], graph: &[Package]) {
    assert_eq!(
        read.len(),
        graph.len(),
        "seed {seed:#x}: one resolved package per block"
    );
    for (resolved, package) in read.iter().zip(graph) {
        assert_eq!(resolved.name(), package.name, "seed {seed:#x}: name");
        assert_eq!(
            resolved.version(),
            package.version,
            "seed {seed:#x}: version"
        );
        assert_eq!(
            resolved.is_local(),
            package.source.is_none(),
            "seed {seed:#x}: {} is local exactly when it has no source",
            package.name
        );
        assert_eq!(
            resolved.checksum(),
            package.checksum.as_deref(),
            "seed {seed:#x}: the checksum is read verbatim"
        );
    }
}

/// A block index drawn from a graph of `len` packages.
fn draw_block(rng: &mut Rng, len: usize) -> Result<usize, Box<dyn Error>> {
    let indices: Vec<usize> = (0..len).collect();
    Ok(*rng.pick_named("blocks", &indices)?)
}

/// Runs the round-trip sweep and returns its replay receipt.
fn lock_trace(seed: u64) -> Result<u64, Box<dyn Error>> {
    let mut rng = Rng::new(seed);
    let mut trace = Trace::new();
    for _ in 0..GRAPHS {
        let graph = draw_graph(&mut rng, 0, 10)?;
        let noisy = rng.coin();
        let rendered = render(&mut rng, &graph, noisy, Fault::None);
        let read = parse(&rendered.text("\n"))?;
        assert_graph(seed, &read, &graph);
        trace.record_number("packages", read.len());
        for resolved in &read {
            trace.record(resolved.name());
            trace.record(resolved.version());
            trace.record_number("local", resolved.is_local());
        }
    }
    Ok(receipt(&trace)?)
}

#[test]
/// A drawn graph rendered as Cargo writes it reads back as exactly that graph.
fn a_rendered_graph_reads_back_as_the_drawn_graph() -> TestResult {
    for seed in SEEDS {
        let mut rng = Rng::new(seed);
        for _ in 0..GRAPHS {
            let graph = draw_graph(&mut rng, 0, 12)?;
            let rendered = render(&mut rng, &graph, false, Fault::None);
            assert_graph(seed, &parse(&rendered.text("\n"))?, &graph);
        }
    }
    Ok(())
}

#[test]
/// Comments, blank lines, indentation, dependency arrays, unknown keys and
/// trailing non-package tables never change the graph that is read.
fn noise_never_changes_the_graph() -> TestResult {
    for seed in SEEDS {
        let mut rng = Rng::new(seed);
        for _ in 0..GRAPHS {
            let graph = draw_graph(&mut rng, 0, 12)?;
            let rendered = render(&mut rng, &graph, true, Fault::None);
            assert_graph(seed, &parse(&rendered.text("\n"))?, &graph);
        }
    }
    Ok(())
}

#[test]
/// CRLF line endings, as a lockfile checked out on Windows has, read the same.
fn crlf_line_endings_read_as_the_same_graph() -> TestResult {
    for seed in SEEDS {
        let mut rng = Rng::new(seed);
        for _ in 0..GRAPHS {
            let graph = draw_graph(&mut rng, 1, 10)?;
            let rendered = render(&mut rng, &graph, true, Fault::None);
            assert_eq!(
                parse(&rendered.text("\r\n"))?,
                parse(&rendered.text("\n"))?,
                "seed {seed:#x}: the line terminator is not part of the graph"
            );
        }
    }
    Ok(())
}

/// The refusal a planted fault must produce, from where the block landed.
type Expect = fn(&Layout, usize) -> Result<LockError, Box<dyn Error>>;

impl Fault {
    /// The key a [`Fault::Duplicate`] assigns twice.
    fn duplicated_key(self) -> Option<&'static str> {
        match self {
            Self::Duplicate(_, key) => Some(key),
            _ => None,
        }
    }
}

/// One drawn graph rendered with a fault planted in one of its blocks.
struct Planted {
    /// The seed the graph was drawn under.
    seed: u64,
    /// The drawn graph.
    graph: Vec<Package>,
    /// The lockfile as rendered with the fault.
    rendered: Rendered,
    /// The block the fault was planted in.
    block: usize,
    /// The planted fault.
    fault: Fault,
}

/// Plants the fault `plant` draws in a drawn block of every drawn graph and
/// hands each rendering to `check`.
fn sweep_planted(
    mut plant: impl FnMut(&mut Rng, usize) -> Result<Fault, Box<dyn Error>>,
    mut check: impl FnMut(&Planted) -> TestResult,
) -> TestResult {
    for seed in SEEDS {
        let mut rng = Rng::new(seed);
        for _ in 0..GRAPHS {
            let graph = draw_graph(&mut rng, 1, 10)?;
            let block = draw_block(&mut rng, graph.len())?;
            let fault = plant(&mut rng, block)?;
            let noisy = rng.coin();
            let rendered = render(&mut rng, &graph, noisy, fault);
            check(&Planted {
                seed,
                graph,
                rendered,
                block,
                fault,
            })?;
        }
    }
    Ok(())
}

/// Plants the fault `make_fault` names in a drawn block and asserts the
/// refusal `expect` derives from the layout.
fn assert_planted_refusal(make_fault: fn(usize) -> Fault, expect: Expect) -> TestResult {
    sweep_planted(
        |_, block| Ok(make_fault(block)),
        |planted| {
            assert_eq!(
                parse(&planted.rendered.text("\n")),
                Err(expect(&planted.rendered.layout, planted.block)?),
                "seed {:#x}: block {} is refused at its own line",
                planted.seed,
                planted.block
            );
            Ok(())
        },
    )
}

/// The nameless refusal at `block`'s header.
fn nameless_at(layout: &Layout, block: usize) -> Result<LockError, Box<dyn Error>> {
    Ok(LockError::NamelessPackage {
        line: layout.header(block)?,
    })
}

/// The versionless refusal at `block`'s header.
fn versionless_at(layout: &Layout, block: usize) -> Result<LockError, Box<dyn Error>> {
    Ok(LockError::VersionlessPackage {
        line: layout.header(block)?,
    })
}

#[test]
/// A block with no `name` line is refused at its header, never skipped.
fn a_block_with_no_name_is_refused_at_its_header() -> TestResult {
    assert_planted_refusal(Fault::OmitName, nameless_at)
}

#[test]
/// A block with no `version` line is refused at its header rather than read
/// with a version no line declared.
fn a_block_with_no_version_is_refused_at_its_header() -> TestResult {
    assert_planted_refusal(Fault::OmitVersion, versionless_at)
}

#[test]
/// A quoted `"name"` key is the same TOML key as the bare one (#543), so a block
/// whose name key is quoted reads back as the drawn graph.
fn a_quoted_name_key_is_read_as_the_name() -> TestResult {
    sweep_planted(
        |_, block| Ok(Fault::QuoteNameKey(block)),
        |planted| {
            let read = parse(&planted.rendered.text("\n"))?;
            assert_graph(planted.seed, &read, &planted.graph);
            Ok(())
        },
    )
}

#[test]
/// A bare, unquoted value is outside the subset the reader accepts, so it is
/// not guessed into a name.
fn a_bare_name_value_is_not_guessed_into_a_name() -> TestResult {
    assert_planted_refusal(Fault::BareNameValue, nameless_at)
}

#[test]
/// Every audited key assigned twice is refused at the second assignment.
fn an_audited_key_assigned_twice_is_refused_at_the_second_line() -> TestResult {
    sweep_planted(
        |rng, block| {
            Ok(Fault::Duplicate(
                block,
                rng.pick_named("audited keys", &AUDITED)?,
            ))
        },
        |planted| {
            let key = planted
                .fault
                .duplicated_key()
                .ok_or("the fault is not a duplicate")?;
            let (seed, block, layout) = (planted.seed, planted.block, &planted.rendered.layout);
            let line = layout.duplicate.ok_or("the duplicate was not planted")?;
            let read = parse(&planted.rendered.text("\n"));
            if layout.declares(block, key, usize::MAX) {
                assert_eq!(
                    read,
                    Err(LockError::DuplicateKey {
                        key: key.to_owned(),
                        line
                    }),
                    "seed {seed:#x}: {key} twice in block {block}"
                );
            } else {
                // The first assignment of a key the block never declared is
                // simply that key: a `source` on a local package, say.
                assert!(
                    read.is_ok(),
                    "seed {seed:#x}: a first {key} is not a duplicate: {read:?}"
                );
            }
            Ok(())
        },
    )
}

#[test]
/// Every line-boundary truncation either reads the blocks it still holds, each
/// with only the fields its remaining lines declare, or refuses the last block
/// at its header; it never invents a field.
fn a_truncated_lockfile_never_invents_a_field() -> TestResult {
    for seed in SEEDS {
        let mut rng = Rng::new(seed);
        for _ in 0..6 {
            let graph = draw_graph(&mut rng, 1, 6)?;
            let rendered = render(&mut rng, &graph, false, Fault::None);
            for cut in 0..=rendered.lines.len() {
                let kept = rendered.lines.get(..cut).ok_or("cut past the end")?;
                let read = parse(&kept.join("\n"));
                assert_truncation(seed, cut, &read, &rendered.layout, graph.len())?;
            }
        }
    }
    Ok(())
}

/// The answer a lockfile cut after line `cut` must produce.
fn assert_truncation(
    seed: u64,
    cut: usize,
    read: &Result<Vec<Resolved>, LockError>,
    layout: &Layout,
    blocks: usize,
) -> TestResult {
    let kept: Vec<usize> = (0..blocks)
        .filter(|block| layout.headers.get(*block).is_some_and(|line| *line <= cut))
        .collect();
    if let Some(&last) = kept.last() {
        if !layout.declares(last, "name", cut) {
            assert_eq!(
                *read,
                Err(nameless_at(layout, last)?),
                "seed {seed:#x}: cut {cut} leaves block {last} without a name"
            );
            return Ok(());
        }
        if !layout.declares(last, "version", cut) {
            assert_eq!(
                *read,
                Err(versionless_at(layout, last)?),
                "seed {seed:#x}: cut {cut} leaves block {last} without a version"
            );
            return Ok(());
        }
    }
    let packages = read
        .as_ref()
        .map_err(|error| format!("cut {cut}: {error}"))?;
    assert_eq!(packages.len(), kept.len(), "seed {seed:#x}: cut {cut}");
    for (resolved, block) in packages.iter().zip(&kept) {
        assert_eq!(
            resolved.is_local(),
            !layout.declares(*block, "source", cut),
            "seed {seed:#x}: cut {cut} block {block}: local follows the lines kept"
        );
        assert_eq!(
            resolved.checksum().is_some(),
            layout.declares(*block, "checksum", cut),
            "seed {seed:#x}: cut {cut} block {block}: no checksum without its line"
        );
    }
    Ok(())
}

#[test]
/// A non-package table, even one with `name` and `version` keys, adds nothing.
fn a_non_package_table_adds_no_package() -> TestResult {
    for seed in SEEDS {
        let mut rng = Rng::new(seed);
        for _ in 0..GRAPHS {
            let graph = draw_graph(&mut rng, 0, 6)?;
            let mut rendered = render(&mut rng, &graph, false, Fault::None);
            rendered.push("[[patch.unused]]".to_owned());
            rendered.push(format!("name = \"{}\"", *rng.pick_named("names", &NAMES)?));
            rendered.push("version = \"0.0.1\"".to_owned());
            assert_graph(seed, &parse(&rendered.text("\n"))?, &graph);
        }
    }
    Ok(())
}

#[test]
/// Each refusal names the line to repair, so an operator can find the block.
fn every_refusal_names_its_line() -> TestResult {
    let mut rng = Rng::new(SEEDS[0]);
    for _ in 0..GRAPHS {
        let line = usize::try_from(rng.between(1, 5_000))?;
        assert_eq!(
            LockError::NamelessPackage { line }.to_string(),
            format!("[[package]] block at line {line} has no name"),
            "the nameless refusal names line {line}"
        );
        assert_eq!(
            LockError::VersionlessPackage { line }.to_string(),
            format!("[[package]] block at line {line} has no version"),
            "the versionless refusal names line {line}"
        );
        assert_eq!(
            LockError::DuplicateKey {
                key: "name".to_owned(),
                line
            }
            .to_string(),
            format!("line {line}: \"name\" is assigned twice in one [[package]] block"),
            "the duplicate refusal names line {line}"
        );
    }
    Ok(())
}

#[test]
/// The replay oracle: one seed, one receipt.
fn the_same_seed_replays_to_the_same_lock_receipt() -> TestResult {
    for seed in SEEDS {
        assert_eq!(
            lock_trace(seed)?,
            lock_trace(seed)?,
            "seed {seed:#x}: the same seed must replay"
        );
    }
    Ok(())
}

#[test]
/// The divergence oracle: a sweep that ignored its seed would pass the replay.
fn distinct_lock_seeds_diverge() -> TestResult {
    assert_ne!(
        lock_trace(SEEDS[0])?,
        lock_trace(SEEDS[1])?,
        "two seeds drew the same graphs"
    );
    Ok(())
}
