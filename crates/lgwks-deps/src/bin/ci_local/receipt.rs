//! The run's record: which tree it describes, the table it prints, and the
//! receipt it writes (issue #127, defect 4).

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use super::lanes::{Lane, Manifest};
use super::utc::stamp;
use super::{LaneResult, Status};

/// The tree a run describes.
pub(super) struct TreeIdentity {
    /// `HEAD`, or `unknown`.
    pub(super) commit: String,
    /// `HEAD^{tree}`, or `unknown`.
    pub(super) tree: String,
    /// A digest of the index.
    pub(super) digest: String,
    /// Modified or staged entries.
    pub(super) dirty: Vec<String>,
    /// Untracked entries.
    pub(super) untracked: Vec<String>,
}

impl TreeIdentity {
    /// Whether the tree is clean and the commit known.
    pub(super) fn verified(&self) -> bool {
        self.dirty.is_empty() && self.untracked.is_empty() && self.commit != "unknown"
    }
}

/// What `git` prints for `args` in `root`, if it succeeds.
pub(super) fn git(root: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        None
    }
}

/// What `git` prints for `args`, or nothing when it fails.
fn git_text(root: &Path, args: &[&str]) -> String {
    match git(root, args) {
        Some(text) if !text.is_empty() => text,
        Some(_) | None => String::new(),
    }
}

/// Git's object hash of `text`, as a stable digest of it.
fn digest_of(root: &Path, text: &str) -> Option<String> {
    let mut child = Command::new("git")
        .args(["hash-object", "--stdin"])
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut input = child.stdin.take()?;
    if let Err(error) = input.write_all(text.as_bytes()) {
        lgwks_std::trace::debug!(%error, "the index listing could not be hashed");
    }
    drop(input);
    let output = child.wait_with_output().ok()?;
    let printed = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if printed.is_empty() {
        None
    } else {
        Some(printed.chars().take(16).collect())
    }
}

/// A value, or `unknown` when git could not say.
fn trimmed_or_unknown(value: Option<String>) -> String {
    match value.map(|text| text.trim().to_owned()) {
        Some(text) if !text.is_empty() => text,
        Some(_) | None => "unknown".to_owned(),
    }
}

/// Reads `git status --porcelain -uall -z` into dirty and untracked entries.
fn read_status(status: &str) -> (Vec<String>, Vec<String>) {
    let mut dirty = Vec::new();
    let mut untracked = Vec::new();
    let mut records = status.split('\0').filter(|record| !record.is_empty());
    while let Some(record) = records.next() {
        let code: String = record.chars().take(2).collect();
        let name: String = record.chars().skip(3).collect();
        if code == "??" {
            untracked.push(name);
            continue;
        }
        if code.starts_with(['R', 'C']) {
            records.next();
        }
        dirty.push(format!("{} {name}", code.trim()).trim().to_owned());
    }
    (dirty, untracked)
}

/// The identity of the tree under `root`.
pub(super) fn tree_identity(root: &Path) -> TreeIdentity {
    let listed = git_text(root, &["ls-files", "-s"]);
    let (dirty, untracked) =
        read_status(&git_text(root, &["status", "--porcelain", "-uall", "-z"]));
    TreeIdentity {
        commit: trimmed_or_unknown(git(root, &["rev-parse", "HEAD"])),
        tree: trimmed_or_unknown(git(root, &["rev-parse", "HEAD^{tree}"])),
        digest: match digest_of(root, &listed) {
            Some(found) => found,
            None => "unknown".to_owned(),
        },
        dirty,
        untracked,
    }
}

/// The output of `argv`, or `missing`.
pub(super) fn capture_line(argv: &[&str], root: &Path) -> String {
    let Some(program) = argv.first() else {
        return "missing".to_owned();
    };
    let output = Command::new(program)
        .args(argv.iter().skip(1))
        .current_dir(root)
        .stdin(Stdio::null())
        .output();
    match output {
        Ok(done) => {
            let printed = String::from_utf8_lossy(&done.stdout).trim().to_owned();
            if printed.is_empty() {
                let errored = String::from_utf8_lossy(&done.stderr).trim().to_owned();
                if errored.is_empty() {
                    "missing".to_owned()
                } else {
                    errored
                }
            } else {
                printed
            }
        }
        Err(_) => "missing".to_owned(),
    }
}

/// The host, as `OS ARCH`.
pub(super) fn host() -> String {
    format!("{} {}", std::env::consts::OS, std::env::consts::ARCH)
}

/// The counts of results by status.
pub(super) fn counts_line(results: &[LaneResult]) -> String {
    let count = |wanted: Status| {
        results
            .iter()
            .filter(|result| result.status == wanted)
            .count()
    };
    format!(
        "passed: {}   failed: {}   blocked: {}   skipped: {}",
        count(Status::Pass),
        count(Status::Fail),
        count(Status::Blocked),
        count(Status::Skip)
    )
}

/// The table printed at the end of a run.
pub(super) fn format_table(results: &[LaneResult]) -> String {
    let mut lines = vec![
        String::new(),
        "─".repeat(60),
        format!("{:<22} {:<8} {:>7}  detail", "lane", "result", "secs"),
    ];
    for result in results {
        let detail: String = result.detail.replace('\n', " ").chars().take(60).collect();
        lines.push(format!(
            "{:<22} {:<8} {:>7.1}  {detail}",
            result.lane_id,
            result.status.label(),
            result.seconds
        ));
    }
    lines.push(String::new());
    lines.push(counts_line(results));
    lines.join("\n")
}

/// Whether a lane belongs to this surface.
fn is_local(lane: &Lane) -> bool {
    lane.surfaces.iter().any(|name| name == "local")
}

/// The Markdown receipt of a run.
pub(super) fn build_receipt(
    root: &Path,
    manifest: &Manifest,
    results: &[LaneResult],
    identity: &TreeIdentity,
    mode: &str,
) -> String {
    let mut lines = vec![
        "# Local gate receipt".to_owned(),
        String::new(),
        format!("Recorded: {}", stamp()),
        format!("Mode: {mode}"),
        format!("Host: {}", host()),
        format!("rustc: {}", capture_line(&["rustc", "--version"], root)),
        format!("Toolchain pin: rust {}", manifest.rust),
        String::new(),
        "## Input identity".to_owned(),
        String::new(),
        format!("- commit: `{}`", identity.commit),
        format!("- tree: `{}`", identity.tree),
        format!("- index digest: `{}`", identity.digest),
    ];
    if identity.verified() {
        lines.push("- identity: **verified** (clean index, no untracked files)".to_owned());
    } else {
        lines.push(format!(
            "- identity: **UNVERIFIED** ({} modified/staged, {} untracked)",
            identity.dirty.len(),
            identity.untracked.len()
        ));
        lines.extend(
            identity
                .dirty
                .iter()
                .map(|entry| format!("  - changed: `{entry}`")),
        );
        lines.extend(
            identity
                .untracked
                .iter()
                .map(|entry| format!("  - untracked: `{entry}`")),
        );
    }
    lines.extend([
        String::new(),
        "This receipt describes the working tree above, not a commit, unless".to_owned(),
        "identity is verified. A lane that did not run on this surface is not".to_owned(),
        "evidence for it.".to_owned(),
        String::new(),
        "## Lanes run on this surface".to_owned(),
        String::new(),
        "| Lane | Result | Started | Ended | Detail |".to_owned(),
        "|---|---|---|---|---|".to_owned(),
    ]);
    for lane in manifest.lanes.iter().filter(|lane| is_local(lane)) {
        if let Some(result) = results.iter().find(|found| found.lane_id == lane.id) {
            let detail: String = result
                .detail
                .replace('|', "\\|")
                .replace('\n', " ")
                .chars()
                .take(200)
                .collect();
            lines.push(format!(
                "| {} | **{}** | {} | {} | {detail} |",
                lane.id,
                result.status.label(),
                result.started,
                result.ended
            ));
        }
    }
    lines.extend([
        String::new(),
        "## Not covered by this surface".to_owned(),
        String::new(),
    ]);
    let uncovered: Vec<&Lane> = manifest
        .lanes
        .iter()
        .filter(|lane| !is_local(lane))
        .collect();
    if uncovered.is_empty() {
        lines.push("- none".to_owned());
    }
    for lane in uncovered {
        let reason = if lane.reason.is_empty() {
            format!("declared surfaces: {}", lane.surfaces.join(", "))
        } else {
            lane.reason.clone()
        };
        lines.push(format!("- `{}` — {reason}", lane.id));
    }
    lines.extend([String::new(), counts_line(results), String::new()]);
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::read_status;

    #[test]
    fn porcelain_status_splits_dirty_from_untracked_and_skips_a_rename_source() {
        let (dirty, untracked) =
            read_status(" M keep.txt\0?? scratch.txt\0R  new.txt\0old.txt\0A  added.txt\0");
        assert_eq!(
            dirty,
            vec!["M keep.txt", "R new.txt", "A added.txt"],
            "changed entries"
        );
        assert_eq!(untracked, vec!["scratch.txt"], "untracked entries");
    }
}
