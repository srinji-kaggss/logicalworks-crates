//! `lgwks-ast script map|check|census [--json] [--top N] [PATH...]`: every
//! `script!` in a tree, read by the one parser (SL-2, #384);
//! `lgwks-ast script diff <base> <head> [--json]`, the architecture change
//! between two checked-out source roots (#655); and
//! `lgwks-ast scan [--json] [--rule NAME...] [PATH...]`, the zero-gate source
//! detectors (INV-SCAN-ZERO, #408), narrowed to the detectors a `--rule` names
//! (#699).
//!
//! `map` prints each invocation's architecture map, written exactly as the
//! `ARCHITECTURE` the macro emits for it renders. `check` prints only what the
//! language refuses, with the message `cargo build` reports at the same line
//! and column. `census` prints the plain-Rust shapes that recur across flows,
//! the evidence a new word is admitted on (SL-7, #389). All three read through
//! `lgwks_ast::script::read_source`, which lexes each file with the tokenizer
//! the macro receives its input from and hands every invocation to
//! `lgwks_ast::script::parse`, so the tool and the compiler cannot disagree
//! about a script: there is no second reader to drift.
//!
//! A `PATH` that is a directory is walked for `.rs` files, skipping build
//! output and vendored trees; a file is read as given; no `PATH` means `.`.
//! `--json` writes one document to stdout for an agent (`map`, `check` and
//! `diff`); `--top` bounds the rows a census prints. The exit status is 0 when
//! nothing is refused, 2 when a script is refused or a file cannot be read, and 2 with
//! the usage for an unknown command. A census measures and never gates, so it
//! exits 0 and says how many scripts it could not read. `scan` has its own
//! contract, written in `scan.rs`: 0 clean, 1 findings on a tree that was read,
//! 2 a file that could not be read, and 64 a command line it could not read.
//!
//! `diff` takes two source roots — two directories, or two files — that a
//! caller checked out, and reads both from disk: there is no git call in the
//! command. It exits 0 when nothing widens or every widening is approved, 1
//! when a widening has no `// lgwks-architecture-approved: <reason>` line in
//! the head source, and 2 for a usage or parse error.

mod census;
mod diff;
#[cfg(feature = "scan")]
mod scan;

use std::fmt;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[cfg(feature = "scan")]
use lgwks_ast::scan::Rule;
use lgwks_ast::script::{Invocation, read_source};
use lgwks_std::fs::{
    FileKind, OmissionAction, OmissionPolicy, OmissionStage, WalkFailure, WalkIter, WalkOptions,
    WalkStreamEntry, walk_dir_iter,
};
use lgwks_std::json::{Map, Value};

/// The command line, shown on a usage error.
const USAGE: &str = "usage: lgwks-ast script map|check|census|diff [--json] [--top N] [PATH...]\n\
       lgwks-ast script diff <base> <head> [--json]\n\
       lgwks-ast scan [--json] [--rule NAME...] [PATH...]\n\
\n\
  map     print every script!'s architecture map, as its ARCHITECTURE renders\n\
  check   print only what the script! language refuses; exit 2 if anything is\n\
  census  print the plain-Rust shapes that recur across flows, most recurrent first\n\
  diff    classify the architecture change between two checked-out source roots;\n\
          exit 1 on a widening with no `// lgwks-architecture-approved: <reason>`\n\
          line in the head source\n\
  scan    run the zero-gate detectors (error swallow, unlogged returns, lint\n\
          allowances, try chains, docs); exit 0 when the tree is clean, 1 when\n\
          findings stand against a tree every file of which was read, 2 when a\n\
          file could not be read, 64 when the command line is wrong\n\
  --json  one JSON document on stdout instead of text (map, check, diff and scan)\n\
  --top   how many shapes a census prints\n\
  --rule  run only the named scan detector, repeatable: ERROR-SWALLOW,\n\
          unlogged-err-return, ALLOW-SILENCE, long-try-chain, tautological-doc\n\
  PATH    a directory to walk for .rs files (and, for scan, Cargo.toml), or one\n\
          file; default `.`\n";

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

/// The status for a command line the tool could not read: EX_USAGE, and the
/// one `scan` answers with when its arguments are wrong (#698). The `script`
/// verbs keep 2 for a misuse because their consumers already map 2 to a
/// refused script, so their contract is unchanged.
const EXIT_USAGE: u8 = 64;

/// The status a misuse of `verb` exits with.
const fn usage_exit(verb: Verb) -> u8 {
    match verb {
        Verb::Scan => EXIT_USAGE,
        Verb::Map | Verb::Check | Verb::Census | Verb::Diff => 2,
    }
}

/// What the command was asked to print.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Verb {
    /// Every invocation's map and every refusal.
    Map,
    /// Only the refusals.
    Check,
    /// The plain-Rust shapes that recur, and where.
    Census,
    /// The architecture change between two source roots.
    Diff,
    /// Every finding of the zero-gate detectors.
    Scan,
}

impl Verb {
    /// The verb as it is typed.
    const fn word(self) -> &'static str {
        match self {
            Self::Map => "map",
            Self::Check => "check",
            Self::Census => "census",
            Self::Diff => "diff",
            Self::Scan => "scan",
        }
    }

    /// How many leading arguments name the verb: `script map` is two.
    const fn words(self) -> usize {
        match self {
            Self::Map | Self::Check | Self::Census | Self::Diff => 2,
            Self::Scan => 1,
        }
    }
}

/// What the arguments after `script <verb>` asked for.
struct Options {
    /// `--json`: one document instead of text.
    json: bool,
    /// `--top`: the rows a census prints.
    top: usize,
    /// `--rule`: the scan detectors to run, as they were typed; empty means
    /// every one of them.
    rules: Vec<String>,
    /// The paths to read; `.` when none was named.
    roots: Vec<PathBuf>,
}

/// Read the arguments after `script <verb>`.
///
/// # Errors
///
/// The first argument `verb` does not take, as the message printed above the
/// usage.
fn options(verb: Verb, rest: &[String]) -> Result<Options, String> {
    let mut parsed = Options {
        json: false,
        top: census::DEFAULT_TOP,
        rules: Vec::new(),
        roots: Vec::new(),
    };
    let mut arguments = rest.iter();
    while let Some(argument) = arguments.next() {
        take(verb, argument, &mut arguments, &mut parsed)?;
    }
    if parsed.roots.is_empty() {
        parsed.roots.push(PathBuf::from("."));
    }
    Ok(parsed)
}

/// Apply one argument to `parsed`, taking its value from `rest` when it has one.
///
/// # Errors
///
/// The message printed above the usage when `verb` does not take `argument`.
fn take(
    verb: Verb,
    argument: &str,
    rest: &mut std::slice::Iter<'_, String>,
    parsed: &mut Options,
) -> Result<(), String> {
    match argument {
        "--json" if verb != Verb::Census => {
            parsed.json = true;
            Ok(())
        }
        "--top" if verb == Verb::Census => {
            let value = rest
                .next()
                .ok_or_else(|| "--top needs a row count".to_owned())?;
            parsed.top = census::rows(value)?;
            Ok(())
        }
        "--rule" if verb == Verb::Scan => {
            let value = rest
                .next()
                .ok_or_else(|| "--rule needs a detector name".to_owned())?;
            parsed.rules.push(value.clone());
            Ok(())
        }
        unknown if unknown.starts_with("--") => Err(format!("unknown option `{unknown}`")),
        path => {
            parsed.roots.push(PathBuf::from(path));
            Ok(())
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

/// Parse the arguments and run `script map`, `script check`, `script census`,
/// `script diff` or `scan`.
fn run(args: &[String], out: &mut impl Write, err: &mut impl Write) -> io::Result<ExitCode> {
    let verb = match (
        args.first().map(String::as_str),
        args.get(1).map(String::as_str),
    ) {
        (Some("script"), Some("map")) => Verb::Map,
        (Some("script"), Some("check")) => Verb::Check,
        (Some("script"), Some("census")) => Verb::Census,
        (Some("script"), Some("diff")) => Verb::Diff,
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
    let parsed = match options(verb, rest) {
        Ok(parsed) => parsed,
        Err(message) => {
            writeln!(err, "lgwks-ast: {message}\n")?;
            write!(err, "{USAGE}")?;
            return Ok(ExitCode::from(usage_exit(verb)));
        }
    };
    if verb == Verb::Scan {
        return run_scan(&parsed.roots, parsed.json, &parsed.rules, out, err);
    }
    if verb == Verb::Diff {
        return run_diff(&parsed, out, err);
    }
    let mut readings = Vec::new();
    for root in &parsed.roots {
        match files_under(root, Wanted::Rust) {
            Ok(files) => readings.extend(files.into_iter().map(read_file)),
            Err(error) => readings.push(FileReading {
                path: root.clone(),
                outcome: Err(error.to_string()),
            }),
        }
    }
    if verb == Verb::Census {
        census::write(&readings, parsed.top, out)
    } else if parsed.json {
        write_json(verb, &readings, out)
    } else {
        write_text(verb, &readings, out, err)
    }
}

/// `lgwks-ast script diff <base> <head>`, which needs exactly two roots.
fn run_diff(parsed: &Options, out: &mut impl Write, err: &mut impl Write) -> io::Result<ExitCode> {
    let (Some(base), Some(head)) = (parsed.roots.first(), parsed.roots.get(1)) else {
        writeln!(
            err,
            "lgwks-ast: `script diff` takes two source roots, <base> and <head>\n"
        )?;
        write!(err, "{USAGE}")?;
        return Ok(ExitCode::from(2));
    };
    if parsed.roots.len() != 2 {
        writeln!(
            err,
            "lgwks-ast: `script diff` takes exactly two source roots, <base> and <head>\n"
        )?;
        write!(err, "{USAGE}")?;
        return Ok(ExitCode::from(2));
    }
    diff::run(base, head, parsed.json, out, err)
}

/// `lgwks-ast scan` over `roots`, with every detector unless `names` selects
/// some.
#[cfg(feature = "scan")]
fn run_scan(
    roots: &[PathBuf],
    json: bool,
    names: &[String],
    out: &mut impl Write,
    err: &mut impl Write,
) -> io::Result<ExitCode> {
    let rules = if names.is_empty() {
        Rule::ALL.to_vec()
    } else {
        let mut selected = Vec::with_capacity(names.len());
        for name in names {
            match Rule::from_name(name) {
                Some(rule) => selected.push(rule),
                None => {
                    writeln!(err, "lgwks-ast: `{name}` names no scan detector\n")?;
                    write!(err, "{USAGE}")?;
                    return Ok(ExitCode::from(EXIT_USAGE));
                }
            }
        }
        selected
    };
    scan::run(roots, json, &rules, out, err)
}

/// Without the `scan` feature the verb exists and says what it needs: asking a
/// build for a capability it does not hold is a misuse of that build, so the
/// status is the usage one rather than a verdict on any tree.
#[cfg(not(feature = "scan"))]
fn run_scan(
    _roots: &[PathBuf],
    _json: bool,
    _names: &[String],
    _out: &mut impl Write,
    err: &mut impl Write,
) -> io::Result<ExitCode> {
    writeln!(err, "lgwks-ast: scan needs the `scan` feature")?;
    Ok(ExitCode::from(EXIT_USAGE))
}

/// Which files a walk keeps.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Wanted {
    /// Every `.rs` file: the input of the script verbs.
    Rust,
    /// Every `.rs` file for the source scan, and with `manifests` every
    /// `Cargo.toml` too: where a package lowers a lint for a whole table, which
    /// the source scan cannot see (#699). One walk keeps both, so a tree is
    /// listed once however many readers the scan owns.
    #[cfg(feature = "scan")]
    Scan {
        /// Whether manifests are kept as well.
        manifests: bool,
    },
}

impl Wanted {
    /// Whether a walked entry is one of the files this walk keeps.
    fn keeps(self, path: &Path) -> bool {
        let rust = path.extension().is_some_and(|extension| extension == "rs");
        match self {
            Self::Rust => rust,
            #[cfg(feature = "scan")]
            Self::Scan { manifests } => rust || (manifests && is_manifest(path)),
        }
    }
}

/// Whether a path names a package manifest, which is read for its lint tables
/// and not as Rust.
#[cfg(feature = "scan")]
fn is_manifest(path: &Path) -> bool {
    path.file_name().is_some_and(|name| name == "Cargo.toml")
}

/// Why a tree could not be walked, or a file could not be read.
///
/// A caller decides on the cause, so the cause is a value: a root it cannot
/// state is a different answer from a directory it cannot list, and a file past
/// the byte bound is neither. The rendered forms are the ones the tool has
/// always printed, so a reader of the log sees the same words.
#[derive(Debug)]
enum ReadingError {
    /// The root could not be stated or read at all.
    UnreadableRoot {
        /// The root as the caller named it.
        root: PathBuf,
        /// Why it could not be read.
        cause: io::Error,
    },
    /// A directory under the root could not be listed.
    UnlistableRoot {
        /// The root as the caller named it.
        root: PathBuf,
        /// Why the walk stopped.
        cause: io::Error,
    },
    /// A file could not be stated or read, or is not UTF-8.
    UnreadableFile {
        /// The file as the caller named it.
        path: PathBuf,
        /// Why it could not be read.
        cause: io::Error,
    },
    /// A file is past [`MAX_FILE_BYTES`].
    TooLarge {
        /// The file as the caller named it.
        path: PathBuf,
        /// The size it reports.
        length: u64,
    },
}

impl fmt::Display for ReadingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::UnreadableRoot {
                ref root,
                ref cause,
            } => {
                write!(formatter, "cannot read {}: {cause}", root.display())
            }
            Self::UnlistableRoot {
                ref root,
                ref cause,
            } => {
                write!(formatter, "cannot walk {}: {cause}", root.display())
            }
            Self::UnreadableFile {
                ref path,
                ref cause,
            } => {
                write!(formatter, "cannot read {}: {cause}", path.display())
            }
            Self::TooLarge { ref path, length } => write!(
                formatter,
                "{} is {length} bytes, past the {MAX_FILE_BYTES}-byte bound on one source file",
                path.display()
            ),
        }
    }
}

impl std::error::Error for ReadingError {}

/// Every file of `wanted` under `root`, in walk order, or `root` itself when it
/// is such a file.
///
/// The walk streams entries out as the filesystem returns them instead of
/// materializing the tree: one open listing per ancestor, never the tree.
/// A directory the walk admits is withheld by name ([`SKIPPED`]) before the
/// walk lists it, which is the exclusion the predicate walk states by
/// refusing to descend.
///
/// # Errors
///
/// [`ReadingError::UnreadableRoot`] when the root does not exist, and
/// [`ReadingError::UnlistableRoot`] when a directory under it could not be
/// listed: a check that skipped an unreadable directory would report a clean
/// tree it never read.
fn files_under(root: &Path, wanted: Wanted) -> Result<Vec<PathBuf>, ReadingError> {
    let metadata = std::fs::metadata(root).map_err(|cause| {
        let refusal = ReadingError::UnreadableRoot {
            root: root.to_path_buf(),
            cause,
        };
        lgwks_std::trace::debug!(error = %refusal, "files_under: returning an error to the caller");
        refusal
    })?;
    if !metadata.is_dir() {
        return Ok(vec![root.to_path_buf()]);
    }
    let policy = OmissionPolicy::new(
        OmissionAction::Report,
        OmissionAction::Report,
        OmissionAction::Report,
    );
    let mut walk: WalkIter = walk_dir_iter(root, &WalkOptions::default(), policy).map_err(|cause| {
        // A root the walk cannot resolve is the same unreadable root the
        // metadata check above reports; anything else stopped the walk.
        let unreadable = cause
            .get_ref()
            .and_then(|source| source.downcast_ref::<WalkFailure>())
            .is_some_and(|failure| failure.stage() == OmissionStage::RootResolution);
        let refusal = if unreadable {
            ReadingError::UnreadableRoot {
                root: root.to_path_buf(),
                cause,
            }
        } else {
            ReadingError::UnlistableRoot {
                root: root.to_path_buf(),
                cause,
            }
        };
        lgwks_std::trace::debug!(error = %refusal, "files_under: returning an error to the caller");
        refusal
    })?;
    let mut kept = Vec::new();
    while let Some(item) = walk.next() {
        // A strict walk refuses on the first omission; the stream reports
        // each one as an item, so the first reported one ends the walk the
        // same way.
        let entry: WalkStreamEntry = item.map_err(|failure| ReadingError::UnlistableRoot {
            root: root.to_path_buf(),
            cause: io::Error::new(failure.source_error().kind(), failure),
        })?;
        if entry.file_type().is_dir()
            && entry
                .path()
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| SKIPPED.contains(&name))
        {
            walk.skip_children();
            continue;
        }
        if entry.kind() == FileKind::File && wanted.keeps(entry.path()) {
            // `.` is the default root: name files from it as `src/lib.rs`,
            // not `./src/lib.rs`, so a location pastes into an editor as the
            // compiler's does.
            kept.push(if root == Path::new(".") {
                entry.relative_path().to_path_buf()
            } else {
                root.join(entry.relative_path())
            });
        }
    }
    Ok(kept)
}

/// Read one file and every `script!` in it.
fn read_file(path: PathBuf) -> FileReading {
    let outcome = read_bounded(&path)
        .map_err(|refusal| refusal.to_string())
        .and_then(|text| {
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
/// [`ReadingError::UnreadableFile`] when the file cannot be stated or read, or
/// is not UTF-8, and [`ReadingError::TooLarge`] when it is past the bound.
fn read_bounded(path: &Path) -> Result<String, ReadingError> {
    let length = std::fs::metadata(path)
        .map(|metadata| metadata.len())
        .map_err(|cause| ReadingError::UnreadableFile {
            path: path.to_path_buf(),
            cause,
        })?;
    if length > MAX_FILE_BYTES {
        let refusal = ReadingError::TooLarge {
            path: path.to_path_buf(),
            length,
        };
        lgwks_std::trace::debug!(error = %refusal, "read_bounded: returning an error to the caller");
        return Err(refusal);
    }
    std::fs::read_to_string(path).map_err(|cause| {
        let refusal = ReadingError::UnreadableFile {
            path: path.to_path_buf(),
            cause,
        };
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
