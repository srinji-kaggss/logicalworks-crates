//! `lgwks-ast script map|check [--json] [PATH...]`: every `script!` in a tree,
//! read by the one parser (SL-2, #384); and `lgwks-ast scan [--json] [PATH...]`,
//! the zero-gate source detectors (INV-SCAN-ZERO, #408).
//!
//! `map` prints each invocation's architecture map, written exactly as the
//! `ARCHITECTURE` the macro emits for it renders. `check` prints only what the
//! language refuses, with the message `cargo build` reports at the same line
//! and column. Both read through `lgwks_ast::script::read_source`, which lexes
//! each file with the tokenizer the macro receives its input from and hands
//! every invocation to `lgwks_ast::script::parse`, so the tool and the compiler
//! cannot disagree about a script: there is no second reader to drift.
//!
//! `scan` runs the five detectors of `lgwks_ast::scan` over every file and
//! prints one `path:line: [rule] evidence` line per finding. A file the checked
//! parse refuses (syntax, size, nesting or time) is reported and counted, never
//! passed.
//!
//! A `PATH` that is a directory is walked for `.rs` files, skipping build
//! output and vendored trees; a file is read as given; no `PATH` means `.`.
//! `--json` writes one document to stdout for an agent. The exit status is 0
//! when nothing is refused, 2 when a script is refused, a finding stands or a
//! file cannot be read, and 2 with the usage for an unknown command.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use lgwks_ast::script::{Invocation, read_source};
use lgwks_std::fs::{Descend, FileKind, WalkOptions, walk_dir_entries};
use lgwks_std::json::{Map, Value};

/// The command line, shown on a usage error.
const USAGE: &str = "usage: lgwks-ast script map|check [--json] [PATH...]\n\
       lgwks-ast scan [--json] [PATH...]\n\
\n\
  map    print every script!'s architecture map, as its ARCHITECTURE renders\n\
  check  print only what the script! language refuses; exit 2 if anything is\n\
  scan   run the zero-gate detectors (error swallow, unlogged returns, lint\n\
         allowances, try chains, docs); exit 2 on any finding or refused file\n\
  --json one JSON document on stdout instead of text\n\
  PATH   a directory to walk for .rs files, or one file; default `.`\n";

/// Directory names never walked: build output, vendored and foreign trees,
/// and version-control metadata.
const SKIPPED: [&str; 7] = [
    "target",
    "vendor",
    "node_modules",
    "third_party",
    ".venv",
    ".git",
    ".jj",
];

/// The largest source file read, in bytes. A Rust file past it is refused by
/// name rather than read into memory whole: no hand-written source in a
/// repository is this large, and a generated one is not where scripts live.
const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;

/// The exit status when a write to stdout or stderr fails for a reason other
/// than a closed pipe.
const EXIT_IO: u8 = 74;

/// What the command was asked to print.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Verb {
    /// Every invocation's map and every refusal.
    Map,
    /// Only the refusals.
    Check,
    /// Every finding of the zero-gate detectors.
    Scan,
}

impl Verb {
    /// The verb as it is typed.
    const fn word(self) -> &'static str {
        match self {
            Self::Map => "map",
            Self::Check => "check",
            Self::Scan => "scan",
        }
    }

    /// How many leading arguments name the verb: `script map` is two.
    const fn words(self) -> usize {
        match self {
            Self::Map | Self::Check => 2,
            Self::Scan => 1,
        }
    }
}

/// One file's reading: its path as the caller named it, and what was found.
struct FileReading {
    /// The path as given on the command line, joined to the walk's relative path.
    path: PathBuf,
    /// The invocations, or why the file could not be read at all.
    outcome: Result<Vec<Invocation>, String>,
}

/// Lock both output handles once and run the command.
fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let stderr = io::stderr();
    let mut err = stderr.lock();
    settle(run(&args, &mut out, &mut err))
}

/// Turn the command's write result into the exit status: a closed pipe is a
/// reader that stopped early, not a failure of the command.
fn settle(result: io::Result<ExitCode>) -> ExitCode {
    match result {
        Ok(code) => code,
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
        Err(_) => ExitCode::from(EXIT_IO),
    }
}

/// Parse the arguments and run `script map`, `script check` or `scan`.
fn run(args: &[String], out: &mut impl Write, err: &mut impl Write) -> io::Result<ExitCode> {
    let verb = match (
        args.first().map(String::as_str),
        args.get(1).map(String::as_str),
    ) {
        (Some("script"), Some("map")) => Verb::Map,
        (Some("script"), Some("check")) => Verb::Check,
        (Some("scan"), _) => Verb::Scan,
        (Some("-h" | "--help" | "help"), _) => {
            write!(out, "{USAGE}")?;
            return Ok(ExitCode::SUCCESS);
        }
        _ => {
            write!(err, "{USAGE}")?;
            return Ok(ExitCode::from(2));
        }
    };
    let Some(rest) = args.get(verb.words()..) else {
        write!(err, "{USAGE}")?;
        return Ok(ExitCode::from(2));
    };
    let json = rest.iter().any(|arg| arg == "--json");
    if let Some(unknown) = rest
        .iter()
        .find(|arg| arg.starts_with("--") && *arg != "--json")
    {
        writeln!(err, "lgwks-ast: unknown option `{unknown}`\n")?;
        write!(err, "{USAGE}")?;
        return Ok(ExitCode::from(2));
    }
    let mut roots: Vec<PathBuf> = rest
        .iter()
        .filter(|arg| !arg.starts_with("--"))
        .map(PathBuf::from)
        .collect();
    if roots.is_empty() {
        roots.push(PathBuf::from("."));
    }
    if verb == Verb::Scan {
        return scan_roots(&roots, json, out, err);
    }
    let mut readings = Vec::new();
    for root in &roots {
        match files_under(root) {
            Ok(files) => readings.extend(files.into_iter().map(read_file)),
            Err(error) => readings.push(FileReading {
                path: root.clone(),
                outcome: Err(error),
            }),
        }
    }
    if json {
        write_json(verb, &readings, out)
    } else {
        write_text(verb, &readings, out, err)
    }
}

/// Every `.rs` file under `root`, in walk order, or `root` itself when it is a
/// file.
///
/// # Errors
///
/// The root does not exist, or a directory under it could not be listed: a
/// check that skipped an unreadable directory would report a clean tree it
/// never read.
fn files_under(root: &Path) -> Result<Vec<PathBuf>, String> {
    let metadata = match std::fs::metadata(root) {
        Ok(metadata) => metadata,
        Err(error) => {
            let refusal = format!("cannot read {}: {error}", root.display());
            lgwks_std::trace::debug!(error = %refusal, "files_under: returning an error to the caller");
            return Err(refusal);
        }
    };
    if !metadata.is_dir() {
        return Ok(vec![root.to_path_buf()]);
    }
    let entries = walk_dir_entries(root, &WalkOptions::default(), None, |path, kind| {
        let skipped = kind == FileKind::Directory
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| SKIPPED.contains(&name));
        if skipped {
            Descend::Skip
        } else {
            Descend::Enter
        }
    })
    .map_err(|error| {
        let refusal = format!("cannot walk {}: {error}", root.display());
        lgwks_std::trace::debug!(error = %refusal, "files_under: returning an error to the caller");
        refusal
    })?;
    Ok(entries
        .iter()
        .filter(|entry| {
            entry.kind() == FileKind::File
                && entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "rs")
        })
        // `.` is the default root: name files from it as `src/lib.rs`, not
        // `./src/lib.rs`, so a location pastes into an editor as the compiler's does.
        .map(|entry| {
            if root == Path::new(".") {
                entry.relative_path().to_path_buf()
            } else {
                root.join(entry.relative_path())
            }
        })
        .collect())
}

/// Read one file and every `script!` in it.
fn read_file(path: PathBuf) -> FileReading {
    let outcome = read_bounded(&path).and_then(|text| {
        read_source(&text).map_err(|refusal| {
            format!(
                "{}:{}:{}: {}",
                path.display(),
                refusal.line(),
                refusal.column().saturating_add(1),
                refusal.message()
            )
        })
    });
    FileReading { path, outcome }
}

/// The file's text, refused past [`MAX_FILE_BYTES`] before it is read.
///
/// # Errors
///
/// The file cannot be stated or read, is not UTF-8, or is larger than the bound.
fn read_bounded(path: &Path) -> Result<String, String> {
    let length = std::fs::metadata(path)
        .map(|metadata| metadata.len())
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    if length > MAX_FILE_BYTES {
        let refusal = format!(
            "{} is {length} bytes, past the {MAX_FILE_BYTES}-byte bound on one source file",
            path.display()
        );
        lgwks_std::trace::debug!(error = %refusal, "read_bounded: returning an error to the caller");
        return Err(refusal);
    }
    std::fs::read_to_string(path).map_err(|error| {
        let refusal = format!("cannot read {}: {error}", path.display());
        lgwks_std::trace::debug!(error = %refusal, "read_bounded: returning an error to the caller");
        refusal
    })
}

/// The counts a summary reports.
#[derive(Default)]
struct Totals {
    /// Files read, whether or not they held a script.
    files: usize,
    /// `script!` invocations found.
    invocations: usize,
    /// Invocations the language refused, plus files that could not be read.
    refused: usize,
}

impl Totals {
    /// Count every reading.
    fn of(readings: &[FileReading]) -> Self {
        let mut totals = Self::default();
        for reading in readings {
            totals.files = totals.files.saturating_add(1);
            match reading.outcome {
                Ok(ref invocations) => {
                    totals.invocations = totals.invocations.saturating_add(invocations.len());
                    let refused = invocations
                        .iter()
                        .filter(|invocation| invocation.read().is_err())
                        .count();
                    totals.refused = totals.refused.saturating_add(refused);
                }
                Err(_) => totals.refused = totals.refused.saturating_add(1),
            }
        }
        totals
    }
}

/// The human form: maps (for `map`) and refusals, then one summary line.
fn write_text(
    verb: Verb,
    readings: &[FileReading],
    out: &mut impl Write,
    err: &mut impl Write,
) -> io::Result<ExitCode> {
    for reading in readings {
        let invocations = match reading.outcome {
            Ok(ref invocations) => invocations,
            Err(ref failure) => {
                writeln!(err, "{failure}")?;
                continue;
            }
        };
        for invocation in invocations {
            match invocation.read() {
                Ok(script) if verb == Verb::Map => {
                    writeln!(
                        out,
                        "{}:{}:{}: script!",
                        reading.path.display(),
                        invocation.line(),
                        invocation.column().saturating_add(1)
                    )?;
                    write!(out, "{script}")?;
                }
                Ok(_) => {}
                Err(refusal) => writeln!(
                    out,
                    "{}:{}:{}: refused: {}",
                    reading.path.display(),
                    refusal.line(),
                    refusal.column().saturating_add(1),
                    refusal.message()
                )?,
            }
        }
    }
    let totals = Totals::of(readings);
    if totals.refused == 0 {
        writeln!(
            out,
            "OK  script {} — {} script! in {} files, nothing refused",
            verb.word(),
            totals.invocations,
            totals.files
        )?;
        Ok(ExitCode::SUCCESS)
    } else {
        writeln!(
            err,
            "REFUSED  script {}: {} refused across {} script! in {} files",
            verb.word(),
            totals.refused,
            totals.invocations,
            totals.files
        )?;
        Ok(ExitCode::from(2))
    }
}

/// The agent form: one JSON document on stdout, the same exit status.
fn write_json(verb: Verb, readings: &[FileReading], out: &mut impl Write) -> io::Result<ExitCode> {
    let mut found = Vec::new();
    let mut unreadable = Vec::new();
    for reading in readings {
        let path = reading.path.display().to_string();
        match reading.outcome {
            Ok(ref invocations) => {
                for invocation in invocations {
                    let mut fields = vec![
                        ("path", Value::from(path.as_str())),
                        ("line", Value::from(invocation.line())),
                        ("column", Value::from(invocation.column().saturating_add(1))),
                    ];
                    match invocation.read() {
                        Ok(script) if verb == Verb::Map => fields.push(("map", script.to_json())),
                        Ok(_) => {}
                        Err(refusal) => fields.push((
                            "refusal",
                            object(vec![
                                ("line", Value::from(refusal.line())),
                                ("column", Value::from(refusal.column().saturating_add(1))),
                                ("message", Value::from(refusal.message())),
                            ]),
                        )),
                    }
                    found.push(object(fields));
                }
            }
            Err(ref failure) => unreadable.push(object(vec![
                ("path", Value::from(path.as_str())),
                ("message", Value::from(failure.as_str())),
            ])),
        }
    }
    let totals = Totals::of(readings);
    let document = object(vec![
        ("verb", Value::from(verb.word())),
        ("files", Value::from(totals.files)),
        ("refused", Value::from(totals.refused)),
        ("invocations", Value::Array(found)),
        ("unreadable", Value::Array(unreadable)),
    ]);
    writeln!(out, "{document}")?;
    Ok(if totals.refused == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(2)
    })
}

/// A JSON object from its fields.
fn object(fields: Vec<(&str, Value)>) -> Value {
    Value::Object(
        fields
            .into_iter()
            .map(|(name, value)| (name.to_owned(), value))
            .collect::<Map<String, Value>>(),
    )
}

/// One file's scan: its path as the caller named it, and what the detectors
/// found, or why the file could not be scanned at all.
#[cfg(feature = "scan")]
struct ScanReading {
    /// The path as given on the command line, joined to the walk's relative path.
    path: PathBuf,
    /// The findings, or the refusal: a file the scan cannot read is never passed.
    outcome: Result<Vec<lgwks_ast::scan::Hit>, String>,
}

/// Scan one file: read it under the byte bound, then run the detectors.
#[cfg(feature = "scan")]
fn scan_file(path: PathBuf) -> ScanReading {
    let outcome = read_bounded(&path).and_then(|text| {
        lgwks_ast::scan::scan_source(&text, &path.display().to_string())
            .map_err(|refusal| refusal.to_string())
    });
    ScanReading { path, outcome }
}

/// `lgwks-ast scan`: every file under `roots` through the five detectors.
///
/// Findings go to stdout one per line as `path:line: [rule] evidence`; a file the
/// scan refuses goes to stderr. Nothing stops at the first problem, so one run
/// names them all, and the exit status is 2 when there is any.
#[cfg(feature = "scan")]
fn scan_roots(
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
    let files = readings.len();
    let findings: usize = readings
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
    if json {
        write_scan_json(&readings, (files, findings, refused), out)?;
    } else {
        write_scan_text(&readings, out, err)?;
        if findings == 0 && refused == 0 {
            writeln!(out, "OK  scan clean — {files} files, zero findings")?;
        } else {
            let unread = if refused == 0 {
                String::new()
            } else {
                format!(", {refused} files refused")
            };
            writeln!(
                err,
                "REFUSED  scan: {findings} findings across {files} files{unread}"
            )?;
        }
    }
    Ok(if findings == 0 && refused == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(2)
    })
}

/// The human form of a scan: one line per finding, one per refused file.
#[cfg(feature = "scan")]
fn write_scan_text(
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

/// The agent form of a scan: one JSON document on stdout. `counts` is the
/// files read, the findings, and the files refused.
#[cfg(feature = "scan")]
fn write_scan_json(
    readings: &[ScanReading],
    counts: (usize, usize, usize),
    out: &mut impl Write,
) -> io::Result<()> {
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
    let (files, findings, refusals) = counts;
    let document = object(vec![
        ("verb", Value::from("scan")),
        ("files", Value::from(files)),
        ("findings", Value::from(findings)),
        ("refused", Value::from(refusals)),
        ("hits", Value::Array(found)),
        ("unscanned", Value::Array(refused)),
    ]);
    writeln!(out, "{document}")
}

/// Without the `scan` feature the verb exists and says what it needs.
#[cfg(not(feature = "scan"))]
fn scan_roots(
    _roots: &[PathBuf],
    _json: bool,
    _out: &mut impl Write,
    err: &mut impl Write,
) -> io::Result<ExitCode> {
    writeln!(err, "lgwks-ast: scan needs the `scan` feature")?;
    Ok(ExitCode::from(2))
}
