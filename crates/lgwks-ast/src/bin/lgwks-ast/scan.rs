//! `lgwks-ast scan [--json] [PATH...]`: the zero-gate source detectors
//! (INV-SCAN-ZERO, #408) over every `.rs` file under the paths named.
//!
//! The five detectors of `lgwks_ast::scan` run through the checked parse, so a
//! file past the byte, node, depth or time bound is refused by name and counted,
//! never passed and never an abort. Nothing stops at the first problem: one run
//! names every finding and every refused file.
//!
//! The exit status is the verdict, and the two failures have two statuses
//! (#698): 0 when the tree is clean, 1 when findings stand against a tree every
//! file of which was read, and 2 when something could not be read, with or
//! without findings beside it. A caller mapping a status to a verdict therefore
//! never reads "the detectors found defects" as "the scan measured nothing".
//! `64` (EX_USAGE) is a command line the tool could not read: the caller's
//! mistake, not the tree's.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use lgwks_ast::scan::{Hit, scan_manifest, scan_source};
use lgwks_std::json::Value;

use super::{Wanted, files_under, object, read_bounded};

/// The status when findings stand against a tree every file of which was read.
const EXIT_FINDINGS: u8 = 1;

/// The status when at least one file could not be read (#698): the same code a
/// `script` verb uses for a refusal, and the one a CI engine already maps to
/// "not measured" rather than to a verdict.
const EXIT_UNREAD: u8 = 2;

/// One file's scan: its path as the caller named it, and what the detectors
/// found, or why the file could not be scanned at all.
struct ScanReading {
    /// The path as given on the command line, joined to the walk's relative path.
    path: PathBuf,
    /// The findings, or the refusal: a file the scan cannot read is never passed.
    outcome: Result<Vec<Hit>, String>,
}

/// The counts a scan reports.
struct Counts {
    /// Files read, whether or not they held a finding.
    files: usize,
    /// Findings across every file.
    findings: usize,
    /// Files the scan refused or could not read.
    refused: usize,
}

/// What the counts mean for the caller's verdict (#698): a tree the detectors
/// read and cleared, a tree they read and refused, or a scan that could not
/// read part of the tree. The third is not a verdict on the tree at all, which
/// is why it is its own status and not the second one with a note attached.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Verdict {
    /// Every file was read and nothing stands against them.
    Clean,
    /// Findings stand against a tree every file of which was read.
    Findings,
    /// At least one file could not be read, whatever else was found.
    Unread,
}

impl Verdict {
    /// The status this verdict exits with.
    fn exit(self) -> ExitCode {
        match self {
            Self::Clean => ExitCode::SUCCESS,
            Self::Findings => ExitCode::from(EXIT_FINDINGS),
            Self::Unread => ExitCode::from(EXIT_UNREAD),
        }
    }

    /// The verdict as the word the text and JSON forms print, so an agent
    /// reading a document and a script reading `$?` name the same thing.
    const fn word(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::Findings => "findings",
            Self::Unread => "unread",
        }
    }
}

impl Counts {
    /// Count every reading.
    fn of(readings: &[ScanReading]) -> Self {
        let findings = readings
            .iter()
            .map(|reading| match reading.outcome {
                Ok(ref hits) => hits.len(),
                Err(_) => 0,
            })
            .sum();
        let refused = readings
            .iter()
            .filter(|reading| reading.outcome.is_err())
            .count();
        Self {
            files: readings.len(),
            findings,
            refused,
        }
    }

    /// What these counts say about the tree.
    const fn verdict(&self) -> Verdict {
        // An unread file outranks a finding: the finding is a fact about a file
        // that was read, while an unread file is the scan saying it does not
        // know what that file holds. Reporting the knowledge while the
        // ignorance is the larger fact would let a caller cache "defects found"
        // where it owes "not measured".
        if self.refused > 0 {
            Verdict::Unread
        } else if self.findings > 0 {
            Verdict::Findings
        } else {
            Verdict::Clean
        }
    }
}

/// Scan one file: read it under the byte bound, then run the detectors.
fn scan_file(path: PathBuf) -> ScanReading {
    let outcome = read_bounded(&path)
        .map_err(|refusal| refusal.to_string())
        .and_then(|text| {
            scan_source(&text, &path.display().to_string()).map_err(|refusal| refusal.to_string())
        });
    ScanReading { path, outcome }
}

/// Scan one manifest: read it under the byte bound, then read its lint tables.
///
/// A manifest that cannot be read is a refusal, not a pass, for the reason a
/// source file is: the scan exists to read what silences a lint, and a table it
/// never saw is one it cannot clear.
fn scan_manifest_file(path: PathBuf) -> ScanReading {
    let outcome = read_bounded(&path)
        .map_err(|refusal| refusal.to_string())
        .map(|text| scan_manifest(&text));
    ScanReading { path, outcome }
}

/// Every reading of `wanted` under `root`: one per file, or one refusal naming
/// the root when the tree could not be walked.
fn read_tree(root: &Path, wanted: Wanted, read: fn(PathBuf) -> ScanReading) -> Vec<ScanReading> {
    match files_under(root, wanted) {
        Ok(files) => files.into_iter().map(read).collect(),
        Err(error) => vec![ScanReading {
            path: root.to_path_buf(),
            outcome: Err(error.to_string()),
        }],
    }
}

/// Run `scan` over `roots`: findings on stdout one per line as
/// `path:line: [rule] evidence`, a refused file on stderr, then the summary.
pub(super) fn run(
    roots: &[PathBuf],
    json: bool,
    out: &mut impl Write,
    err: &mut impl Write,
) -> io::Result<ExitCode> {
    let mut readings = Vec::new();
    for root in roots {
        readings.extend(read_tree(root, Wanted::Rust, scan_file));
        readings.extend(read_tree(root, Wanted::Manifest, scan_manifest_file));
    }
    let counts = Counts::of(&readings);
    if json {
        write_json(&readings, &counts, out)?;
    } else {
        write_text(&readings, out, err)?;
        write_summary(&counts, out, err)?;
    }
    Ok(counts.verdict().exit())
}

/// The closing line: `OK` on stdout for a clean tree, `REFUSED` on stderr when
/// findings stand against a tree that was read, and `UNMEASURED` when part of
/// the tree could not be read. The word is the status in prose, so a reader of
/// the log and a reader of `$?` cannot come to different conclusions.
fn write_summary(counts: &Counts, out: &mut impl Write, err: &mut impl Write) -> io::Result<()> {
    match counts.verdict() {
        Verdict::Clean => writeln!(
            out,
            "OK  scan clean — {} files, zero findings",
            counts.files
        ),
        Verdict::Findings => writeln!(
            err,
            "REFUSED  scan: {} findings across {} files",
            counts.findings, counts.files
        ),
        Verdict::Unread => writeln!(
            err,
            "UNMEASURED  scan: {} of {} files could not be read; {} findings across the rest",
            counts.refused, counts.files, counts.findings
        ),
    }
}

/// The human form of a scan: one line per finding, one per refused file.
fn write_text(
    readings: &[ScanReading],
    out: &mut impl Write,
    err: &mut impl Write,
) -> io::Result<()> {
    for reading in readings {
        match reading.outcome {
            Ok(ref hits) => {
                for hit in hits {
                    writeln!(
                        out,
                        "{}:{}: [{}] {}",
                        reading.path.display(),
                        hit.line(),
                        hit.rule(),
                        hit.snippet()
                    )?;
                }
            }
            Err(ref failure) => writeln!(err, "scan error: {failure}")?,
        }
    }
    Ok(())
}

/// The agent form of a scan: one JSON document on stdout.
fn write_json(readings: &[ScanReading], counts: &Counts, out: &mut impl Write) -> io::Result<()> {
    let mut found = Vec::new();
    let mut refused = Vec::new();
    for reading in readings {
        let path = reading.path.display().to_string();
        match reading.outcome {
            Ok(ref hits) => {
                for hit in hits {
                    found.push(object(vec![
                        ("path", Value::from(path.as_str())),
                        ("line", Value::from(hit.line())),
                        ("rule", Value::from(hit.rule())),
                        ("snippet", Value::from(hit.snippet())),
                    ]));
                }
            }
            Err(ref failure) => refused.push(object(vec![
                ("path", Value::from(path.as_str())),
                ("message", Value::from(failure.as_str())),
            ])),
        }
    }
    let document = object(vec![
        ("verb", Value::from("scan")),
        ("verdict", Value::from(counts.verdict().word())),
        ("files", Value::from(counts.files)),
        ("findings", Value::from(counts.findings)),
        ("refused", Value::from(counts.refused)),
        ("hits", Value::Array(found)),
        ("unscanned", Value::Array(refused)),
    ]);
    writeln!(out, "{document}")
}
