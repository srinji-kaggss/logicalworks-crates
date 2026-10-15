//! `lgwks-ast scan [--json] [PATH...]`: the zero-gate source detectors
//! (INV-SCAN-ZERO, #408) over every `.rs` file under the paths named.
//!
//! The five detectors of `lgwks_ast::scan` run through the checked parse, so a
//! file past the byte, node, depth or time bound is refused by name and counted,
//! never passed and never an abort. Nothing stops at the first problem: one run
//! names every finding and every refused file, and the exit status is 2 when
//! there is any.

use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use lgwks_ast::scan::{Hit, scan_source};
use lgwks_std::json::Value;

use super::{files_under, object, read_bounded};

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

    /// Whether nothing stands against the tree.
    const fn clean(&self) -> bool {
        self.findings == 0 && self.refused == 0
    }
}

/// Scan one file: read it under the byte bound, then run the detectors.
fn scan_file(path: PathBuf) -> ScanReading {
    let outcome = read_bounded(&path).and_then(|text| {
        scan_source(&text, &path.display().to_string()).map_err(|refusal| refusal.to_string())
    });
    ScanReading { path, outcome }
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
        match files_under(root) {
            Ok(files) => readings.extend(files.into_iter().map(scan_file)),
            Err(error) => readings.push(ScanReading {
                path: root.clone(),
                outcome: Err(error),
            }),
        }
    }
    let counts = Counts::of(&readings);
    if json {
        write_json(&readings, &counts, out)?;
    } else {
        write_text(&readings, out, err)?;
        write_summary(&counts, out, err)?;
    }
    Ok(if counts.clean() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(2)
    })
}

/// The closing line: `OK` on stdout for a clean tree, `REFUSED` on stderr with
/// the counts otherwise.
fn write_summary(counts: &Counts, out: &mut impl Write, err: &mut impl Write) -> io::Result<()> {
    if counts.clean() {
        return writeln!(
            out,
            "OK  scan clean — {} files, zero findings",
            counts.files
        );
    }
    let unread = if counts.refused == 0 {
        String::new()
    } else {
        format!(", {} files refused", counts.refused)
    };
    writeln!(
        err,
        "REFUSED  scan: {} findings across {} files{unread}",
        counts.findings, counts.files
    )
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
        ("files", Value::from(counts.files)),
        ("findings", Value::from(counts.findings)),
        ("refused", Value::from(counts.refused)),
        ("hits", Value::Array(found)),
        ("unscanned", Value::Array(refused)),
    ]);
    writeln!(out, "{document}")
}
