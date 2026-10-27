//! The streaming walk against the materializing walk on one fixed tree (#692).
//!
//! Source: POSIX.1-2017 `readdir` names each entry's type in `d_type`, so a
//! walk that needs only the kind pays no `stat` per entry; logical_ci's ADR
//! 0003 records the two `read_dir` loops this crate replaced, one needing no
//! stat at all and one statting on scoped threads after a `d_type`-only walk.
//!
//! The run builds a deterministic tree (branching and depth below), then
//! walks it four ways on the same options and prints one measured line per
//! way: entries admitted, wall time, resident set after the walk, and — for
//! the stream — the time to its first entry, which the materializing walk
//! cannot report before its last directory. The designed syscall counts
//! beside the lines are computed from the admitted shape, not sampled: one
//! `canonicalize` of the root, one `read_dir` per listed directory, one
//! identity stat per entered directory on Unix, and one `lstat` per entry
//! only where the caller asked for metadata. The platform `find` row is a
//! reference, not a counterpart: the Microsoft counterpart
//! (`Get-ChildItem -Recurse`) is not installed on this host.
//!
//! Run it on the release build:
//! `cargo run --release -p lgwks_std --example walk_stream`.
//! The run fails when the walks disagree on the admitted set; it never fails
//! on a timing, because a loaded host is not a regression.
//!
//! One reading (2026-10-26, release, APFS, 5,264 entries in 584 directories):
//! kind-only 12.7 ms at 2,544 KiB with its first entry after 0.5 ms,
//! with-metadata 18.4 ms at 2,576 KiB, materialized 38.1 ms at 4,832 KiB,
//! platform find 23.8 ms. The same admitted set every way.

use std::error::Error;
use std::hint::black_box;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use lgwks_std::fs::{Descend, FileKind, WalkOptions, walk_dir_entries, walk_dir_iter};

/// Branching of the measured tree: each directory holds this many subdirs.
const WIDTH: usize = 8;
/// Depth of the measured tree below the root.
const DEPTH: usize = 3;
/// Files each directory of the measured tree holds.
const FILES_PER_DIR: usize = 8;

/// One walk's measured line.
struct Measurement {
    /// What ran, as the report names it.
    name: &'static str,
    /// Entries the walk admitted.
    entries: usize,
    /// Wall time of the whole walk.
    wall: std::time::Duration,
    /// Time to the walk's first admitted entry, where the walk streams.
    first: Option<std::time::Duration>,
    /// Resident set sampled after the walk, in KiB.
    rss_kib: Option<u64>,
}

/// A scratch tree unique to this process, removed when the run ends.
fn scratch() -> io::Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!("lgwks-walk-stream-{}", std::process::id()));
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Build the fixed tree under `root`: `WIDTH` subdirectories per directory
/// down to `DEPTH`, each holding `FILES_PER_DIR` files. Returns the directory
/// and file counts the designed syscall counts are computed from.
fn build_tree(root: &Path) -> io::Result<(usize, usize)> {
    fn level(dir: &Path, below: usize) -> io::Result<(usize, usize)> {
        for file in 0..FILES_PER_DIR {
            std::fs::write(dir.join(format!("file_{file}.txt")), b"walk")?;
        }
        if below == 0 {
            return Ok((0, FILES_PER_DIR));
        }
        let mut dirs = 0_usize;
        let mut files = FILES_PER_DIR;
        for index in 0..WIDTH {
            let child = dir.join(format!("dir_{index}"));
            std::fs::create_dir(&child)?;
            let (sub_dirs, sub_files) = level(&child, below.saturating_sub(1))?;
            dirs = dirs.saturating_add(sub_dirs.saturating_add(1));
            files = files.saturating_add(sub_files);
        }
        Ok((dirs, files))
    }
    level(root, DEPTH)
}

/// Resident set sampled after a walk, in KiB: peak on Linux through `VmHWM`,
/// current on macOS through `ps`, unavailable elsewhere. `None` where the
/// platform gave no reading, so an absent meter never reports a zero the
/// process did not hold.
fn rss_kib() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
            for line in status.lines() {
                if let Some(rest) = line.strip_prefix("VmHWM:") {
                    let digits: String = rest.chars().filter(char::is_ascii_digit).collect();
                    if let Ok(peak) = digits.parse::<u64>() {
                        return Some(peak);
                    }
                }
            }
        }
        None
    }
    #[cfg(target_os = "macos")]
    {
        let output = std::process::Command::new("/bin/ps")
            .arg("-o")
            .arg("rss=")
            .arg("-p")
            .arg(std::process::id().to_string())
            .output();
        match output {
            Ok(answered) => String::from_utf8_lossy(&answered.stdout)
                .trim()
                .parse::<u64>()
                .ok(),
            Err(_) => None,
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

/// Stream the tree kind-only: no per-entry stat, the `d_type` walk.
fn stream_kinds(root: &Path, options: &WalkOptions) -> Result<Measurement, Box<dyn Error>> {
    let started = Instant::now();
    let mut first = None;
    let mut entries = 0_usize;
    let mut dirs = 0_usize;
    for item in walk_dir_iter(root, options, Default::default())? {
        if first.is_none() {
            first = Some(started.elapsed());
        }
        let entry = item?;
        black_box(entry.path());
        if entry.kind() == FileKind::Directory {
            dirs = dirs.saturating_add(1);
        }
        entries = entries.saturating_add(1);
    }
    black_box(dirs);
    Ok(Measurement {
        name: "stream kind-only",
        entries,
        wall: started.elapsed(),
        first,
        rss_kib: rss_kib(),
    })
}

/// Stream the tree reading every entry's own `lstat`.
fn stream_metadata(root: &Path, options: &WalkOptions) -> Result<Measurement, Box<dyn Error>> {
    let started = Instant::now();
    let mut first = None;
    let mut entries = 0_usize;
    for item in walk_dir_iter(root, options, Default::default())? {
        if first.is_none() {
            first = Some(started.elapsed());
        }
        let entry = item?;
        black_box(entry.metadata()?.len());
        entries = entries.saturating_add(1);
    }
    Ok(Measurement {
        name: "stream with-metadata",
        entries,
        wall: started.elapsed(),
        first,
        rss_kib: rss_kib(),
    })
}

/// Materialize the tree with the entry walk, one `lstat` carried per entry.
fn materialized(root: &Path, options: &WalkOptions) -> io::Result<Measurement> {
    let started = Instant::now();
    let entries = walk_dir_entries(root, options, None, |_, _| Descend::Enter)?;
    let count = entries.len();
    for entry in &entries {
        black_box(entry.path());
    }
    let wall = started.elapsed();
    drop(entries);
    Ok(Measurement {
        name: "materialized entries",
        entries: count,
        wall,
        first: None,
        rss_kib: rss_kib(),
    })
}

/// The platform reference: `/usr/bin/find` over the same tree.
#[cfg(unix)]
fn platform_find(root: &Path) -> io::Result<Measurement> {
    let started = Instant::now();
    let output = std::process::Command::new("/usr/bin/find")
        .arg(root)
        .output()?;
    if !output.status.success() {
        lgwks_std::trace::warn!(
            status = ?output.status,
            "walk-stream: the platform find failed"
        );
        return Err(io::Error::other(format!(
            "the platform find failed: {:?}",
            output.status
        )));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let entries = text.lines().count().saturating_sub(1);
    black_box(&output.stdout);
    Ok(Measurement {
        name: "find (reference)",
        entries,
        wall: started.elapsed(),
        first: None,
        rss_kib: rss_kib(),
    })
}

/// Print one measured line with the designed syscall counts it answers.
fn report(out: &mut impl Write, measured: &Measurement, designed: &str) -> io::Result<()> {
    let mut first = String::new();
    if let Some(at) = measured.first {
        first = format!(" first {at:?}");
    }
    let mut rss = String::from("      n/a");
    if let Some(kib) = measured.rss_kib {
        rss = format!("{kib:>8} KiB");
    }
    writeln!(
        out,
        "  {:<22} entries {:>6}   wall {:>9?}   rss {rss}{first}   designed: {designed}",
        measured.name, measured.entries, measured.wall,
    )
}

/// Walk the fixed tree every way, print the lines, and refuse a disagreement.
fn run(out: &mut impl Write) -> Result<bool, Box<dyn Error>> {
    let root = scratch()?;
    let built = build_tree(&root);
    let (dirs, files) = match built {
        Ok(shape) => shape,
        Err(error) => {
            lgwks_std::trace::warn!(%error, "walk-stream: the fixed tree could not be built");
            let _removed = std::fs::remove_dir_all(&root);
            return Err(error.into());
        }
    };
    let outcome = walk_all(out, &root, dirs, files);
    let _removed = std::fs::remove_dir_all(&root);
    outcome
}

/// The walks over a built tree, so the tree removal runs on every path.
fn walk_all(
    out: &mut impl Write,
    root: &Path,
    dirs: usize,
    files: usize,
) -> Result<bool, Box<dyn Error>> {
    let options = WalkOptions::default();
    let admitted = dirs.saturating_add(files);
    writeln!(
        out,
        "walk-stream: admitted {admitted} entries in {dirs} directories"
    )?;
    let kinds = stream_kinds(root, &options)?;
    // Kind-only: one stat per entered directory on Unix, none per entry.
    report(
        out,
        &kinds,
        &format!(
            "1 canonicalize, {} read_dir, {} identity stat, 0 entry stat",
            dirs.saturating_add(1),
            dirs.saturating_add(1),
        ),
    )?;
    let metadata = stream_metadata(root, &options)?;
    // With metadata: the same directory stats plus one lstat per entry.
    report(
        out,
        &metadata,
        &format!(
            "1 canonicalize, {} read_dir, {} identity stat, {admitted} entry lstat",
            dirs.saturating_add(1),
            dirs.saturating_add(1),
        ),
    )?;
    let entries = materialized(root, &options)?;
    // The entry walk carries one lstat per entry, like the metadata stream.
    report(
        out,
        &entries,
        &format!(
            "1 canonicalize, {} read_dir, {admitted} entry lstat",
            dirs.saturating_add(1),
        ),
    )?;
    #[cfg(unix)]
    {
        let found = platform_find(root)?;
        report(out, &found, "the platform's own walk, for reference")?;
        if found.entries != admitted {
            writeln!(
                out,
                "the platform reference disagrees: find {0} entries, walks {admitted}",
                found.entries
            )?;
            return Ok(false);
        }
    }
    if kinds.entries != admitted || metadata.entries != admitted || entries.entries != admitted {
        writeln!(
            out,
            "the walks disagree: stream {} with-metadata {} materialized {} admitted {admitted}",
            kinds.entries, metadata.entries, entries.entries
        )?;
        return Ok(false);
    }
    writeln!(out, "every walk admits the same entries")?;
    Ok(true)
}

fn main() -> ExitCode {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    match run(&mut out) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(cause) => {
            let _reported = writeln!(out, "walk_stream: {cause}");
            ExitCode::from(2)
        }
    }
}
