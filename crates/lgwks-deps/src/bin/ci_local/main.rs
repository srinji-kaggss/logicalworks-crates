//! Local gate coordinator.
//!
//! Executes the lanes declared in `scripts/gate-lanes.toml`. That file is the one
//! definition of what the gate checks; `check-gate-parity` keeps
//! `.github/workflows/ci.yml` honest against it.
//!
//! Disposition of a lane that cannot run (issue #127, defect 1):
//!
//! * platform mismatch            -> `skip`  (out of scope for this host)
//! * missing need, `required`      -> `blocked` (the gate refuses; never a pass)
//! * missing need, optional        -> `skip`  (named, never counted as `pass`)
//!
//! A green exit is every applicable required lane at `pass`. Skips are listed and
//! are not evidence for the surface that did not run them.
//!
//! Usage, from the repository root:
//!
//! ```text
//! ci-local                 full gate
//! ci-local --fast          the lanes marked fast
//! ci-local --lane ID       one lane (what CI runs per step)
//! ci-local --list          print lane ids and surfaces
//! ci-local --receipt       also write evidence/ci-local-*.md
//! ci-local --jobs 1        one lane at a time, output streamed
//! ci-local --root PATH     gate another checkout
//! ```
//!
//! How a multi-lane run is scheduled, and why (the gate must finish in under five
//! minutes of wall time):
//!
//! * Lanes that share a feature set carry the same `group` in the manifest. Groups
//!   run concurrently, lanes within a group in order. Each group builds in its own
//!   target directory (`<target>/gate-<group>`), because cargo holds one lock per
//!   target directory for a whole build.
//! * Every lane's `TMPDIR` is a RAM-backed volume: the simulation suites write a
//!   real file journal and `sync_all` it, and on a physical disk those syncs queue
//!   behind each other.
//! * A lane's output goes to `<target>/gate-logs/<lane>.log`; a lane that fails
//!   prints the tail of its log and the path.
//!
//! Exit codes: 0 every applicable lane passed; 1 a lane failed or was blocked; 2
//! usage or manifest error.

mod builtins;
#[path = "../shared/fixture.rs"]
mod fixture;
mod invariants;
mod lanes;
mod modules;
mod process;
mod receipt;
mod runners;
mod scratch;
#[path = "../shared/toml_subset.rs"]
mod toml_subset;
#[path = "../shared/utc.rs"]
mod utc;

#[cfg(test)]
mod cli_tests;

use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Mutex;
use std::thread;
use std::time::Instant;

use lanes::{Lane, Manifest, load_lanes, missing_needs, platform_applies, platform_name};
use process::Children;
use receipt::{build_receipt, capture_line, format_table, host, tree_identity};
use scratch::Scratch;
use utc::stamp;

/// How many trailing lines of a failed lane's log are printed.
const LOG_TAIL_LINES: usize = 150;

/// Why the gate could not run.
#[derive(Debug)]
pub(crate) enum GateError {
    /// A file could not be read or written.
    Io {
        /// What was being read.
        path: PathBuf,
        /// What the operating system said.
        cause: io::Error,
    },
    /// The lane table or a manifest is malformed.
    Manifest(String),
    /// The command line is wrong.
    Usage(String),
}

impl fmt::Display for GateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Io { ref path, ref cause } if path.as_os_str().is_empty() => write!(formatter, "{cause}"),
            Self::Io { ref path, ref cause } => write!(formatter, "{}: {cause}", path.display()),
            Self::Manifest(ref what) | Self::Usage(ref what) => write!(formatter, "{what}"),
        }
    }
}

impl std::error::Error for GateError {}

impl From<io::Error> for GateError {
    fn from(cause: io::Error) -> Self {
        Self::Io {
            path: PathBuf::new(),
            cause,
        }
    }
}

/// Logs `error` at the site that produced it, then hands it back as the `Err`.
pub(crate) fn fail<T>(error: GateError, site: &'static str) -> Result<T, GateError> {
    lgwks_std::trace::debug!(error = ?error, site, "returning an error to the caller");
    Err(error)
}

/// Reads a file whole, naming it when it cannot be read.
pub(crate) fn read(path: &Path) -> Result<String, GateError> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(cause) => fail(
            GateError::Io {
                path: path.to_path_buf(),
                cause,
            },
            "read",
        ),
    }
}

/// How a lane ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Status {
    /// It ran and passed.
    Pass,
    /// It ran and failed.
    Fail,
    /// A required prerequisite is missing.
    Blocked,
    /// It does not apply here.
    Skip,
}

impl Status {
    /// The word the table and receipt print.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Blocked => "blocked",
            Self::Skip => "skip",
        }
    }
}

/// One lane's outcome.
pub(crate) struct LaneResult {
    /// The lane.
    pub(crate) lane_id: String,
    /// How it ended.
    pub(crate) status: Status,
    /// When it started.
    pub(crate) started: String,
    /// When it ended.
    pub(crate) ended: String,
    /// What there is to say about it.
    pub(crate) detail: String,
    /// How long it ran.
    pub(crate) seconds: f64,
}

/// A builtin lane's verdict.
pub(crate) struct Verdict {
    /// 0 for a pass.
    pub(crate) code: i32,
    /// What the check found.
    pub(crate) detail: String,
}

/// What a builtin lane runs against.
pub(crate) struct Context<'run> {
    /// The repository root.
    pub(crate) root: &'run Path,
    /// The environment lanes run in.
    pub(crate) env: &'run BTreeMap<String, String>,
    /// The children the gate owns.
    pub(crate) children: &'run Children,
    /// The lane's log, when it has one.
    pub(crate) out: Option<&'run File>,
}

/// The two output streams, locked per line so lanes in different groups interleave whole lines.
pub(crate) struct Console {
    /// Standard output.
    out: Mutex<Box<dyn Write + Send>>,
    /// Standard error.
    err: Mutex<Box<dyn Write + Send>>,
}

impl Console {
    /// A console over the given writers.
    pub(crate) fn new(out: Box<dyn Write + Send>, err: Box<dyn Write + Send>) -> Self {
        Self {
            out: Mutex::new(out),
            err: Mutex::new(err),
        }
    }

    /// Writes a line to standard output.
    fn say(&self, text: &str) {
        Self::line(&self.out, text);
    }

    /// Writes a line to standard error.
    fn warn(&self, text: &str) {
        Self::line(&self.err, text);
    }

    /// Writes a line to one stream.
    fn line(stream: &Mutex<Box<dyn Write + Send>>, text: &str) {
        let mut held = match stream.lock() {
            Ok(held) => held,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Err(error) = writeln!(held, "{text}").and_then(|()| held.flush()) {
            lgwks_std::trace::debug!(%error, "a console line could not be written");
        }
    }
}

/// The environment lanes run in.
fn child_env(scratch: Option<&Path>, target: Option<&Path>) -> BTreeMap<String, String> {
    let mut env: BTreeMap<String, String> = std::env::vars_os()
        .map(|entry| (entry.0.to_string_lossy().into_owned(), entry.1.to_string_lossy().into_owned()))
        .collect();
    let existing = path_of(&env);
    if let Some(home) = env.get("HOME") {
        let cargo = Path::new(home).join(".cargo").join("bin");
        env.insert("PATH".to_owned(), format!("{}:{existing}", cargo.display()));
    }
    env.insert("CARGO_INCREMENTAL".to_owned(), "0".to_owned());
    env.insert("CARGO_TERM_COLOR".to_owned(), "never".to_owned());
    env.entry("RUST_BACKTRACE".to_owned()).or_insert_with(|| "1".to_owned());
    if let Some(directory) = scratch {
        env.insert("TMPDIR".to_owned(), format!("{}/", directory.display()));
    }
    if let Some(directory) = target {
        env.insert("CARGO_TARGET_DIR".to_owned(), directory.display().to_string());
    }
    env
}

/// The `PATH` a lane's needs are checked against.
fn path_of(env: &BTreeMap<String, String>) -> String {
    match env.get("PATH") {
        Some(path) if !path.is_empty() => path.clone(),
        Some(_) | None => String::new(),
    }
}

/// Runs a builtin lane by name.
fn run_builtin(name: &str, context: &Context<'_>) -> Result<Verdict, GateError> {
    match name {
        "unwrap-scan" => builtins::unwrap_scan(context),
        "suppressions" => builtins::suppressions(context),
        "artifacts" => builtins::artifacts(context),
        "invariants" => invariants::invariants(context),
        "contract-drift" => builtins::contract_drift(context),
        "docsrs-metadata" => runners::docsrs_metadata(context),
        "readme-quickstart" => builtins::readme_quickstart(context),
        "debug-e2e" => runners::debug_e2e(context),
        "simulation-evidence" => runners::simulation_evidence(context),
        _ => Ok(Verdict {
            code: 2,
            detail: format!("unknown builtin {name:?}"),
        }),
    }
}

/// Opens a lane's log, making its directory.
fn open_log(path: &Path) -> Result<File, GateError> {
    if let Some(parent) = path.parent()
        && let Err(cause) = fs::create_dir_all(parent)
    {
        return fail(
            GateError::Io {
                path: parent.to_path_buf(),
                cause,
            },
            "open_log",
        );
    }
    match File::create(path) {
        Ok(file) => Ok(file),
        Err(cause) => fail(
            GateError::Io {
                path: path.to_path_buf(),
                cause,
            },
            "open_log",
        ),
    }
}

/// What a lane did, before it is timed and stamped.
struct Outcome {
    /// How it ended.
    status: Status,
    /// What there is to say about it.
    detail: String,
}

/// Runs a lane that applies and has its needs, writing to `log`.
fn execute_lane(
    lane: &Lane,
    root: &Path,
    env: &BTreeMap<String, String>,
    log: Option<&Path>,
    children: &Children,
) -> Result<Outcome, GateError> {
    let file = match log {
        Some(path) => Some(open_log(path)?),
        None => None,
    };
    if let Some(name) = lane.builtin.as_deref() {
        let context = Context {
            root,
            env,
            children,
            out: file.as_ref(),
        };
        let verdict = run_builtin(name, &context)?;
        if let Some(mut sink) = file.as_ref()
            && let Err(error) = writeln!(sink, "{}", verdict.detail)
        {
            lgwks_std::trace::debug!(%error, "a lane's log could not be written");
        }
        let status = if verdict.code == 0 { Status::Pass } else { Status::Fail };
        return Ok(Outcome {
            status,
            detail: verdict.detail,
        });
    }
    let command = match lane.command.as_deref() {
        Some(command) => command.to_owned(),
        None => String::new(),
    };
    let code = children.run(&["bash".to_owned(), "-c".to_owned(), command], root, env, file.as_ref());
    if code == 0 {
        Ok(Outcome {
            status: Status::Pass,
            detail: String::new(),
        })
    } else {
        Ok(Outcome {
            status: Status::Fail,
            detail: format!("exit {code}"),
        })
    }
}

/// Runs one lane, or says why it did not run.
fn run_lane(lane: &Lane, root: &Path, env: &BTreeMap<String, String>, log: Option<&Path>, children: &Children) -> LaneResult {
    let started = stamp();
    let clock = Instant::now();
    let finish = |status: Status, detail: String| LaneResult {
        lane_id: lane.id.clone(),
        status,
        started: started.clone(),
        ended: stamp(),
        detail,
        seconds: clock.elapsed().as_secs_f64(),
    };
    if !platform_applies(lane) {
        return finish(
            Status::Skip,
            format!("out of scope for {}; declared for {:?}", platform_name(), lane.platforms),
        );
    }
    let missing = match missing_needs(lane, root, &path_of(env)) {
        Ok(missing) => missing,
        Err(error) => return finish(Status::Fail, error.to_string()),
    };
    if !missing.is_empty() {
        let status = if lane.required { Status::Blocked } else { Status::Skip };
        let need_list = missing.join(", ");
        let detail = if lane.reason.is_empty() {
            format!("missing prerequisite(s): {need_list}")
        } else {
            format!("missing prerequisite(s): {need_list} ({})", lane.reason)
        };
        return finish(status, detail);
    }
    match execute_lane(lane, root, env, log, children) {
        Ok(outcome) => finish(outcome.status, outcome.detail),
        Err(error) => finish(Status::Fail, format!("the lane could not run: {error}")),
    }
}

/// The last lines of a log.
fn log_tail(log: &Path) -> String {
    match fs::read_to_string(log) {
        Ok(text) => {
            let lines: Vec<&str> = text.lines().collect();
            let from = lines.len().saturating_sub(LOG_TAIL_LINES);
            match lines.get(from..) {
                Some(tail) => tail.join("\n"),
                None => String::new(),
            }
        }
        Err(_) => String::new(),
    }
}

/// Prints a lane's result line, and a failed lane's log.
fn report(console: &Console, result: &LaneResult, log: Option<&Path>) {
    let head = format!("<== {}: {} ({:.1} s)", result.lane_id, result.status.label(), result.seconds);
    let line = match result.detail.lines().next() {
        Some(first) => format!("{head} — {first}"),
        None => head,
    };
    if matches!(result.status, Status::Fail | Status::Blocked) {
        console.warn(&line);
        if let Some(path) = log.filter(|candidate| candidate.exists()) {
            console.warn(&format!("---- last lines of {} ----", path.display()));
            console.warn(&log_tail(path));
        }
    } else {
        console.say(&line);
    }
}

/// The directory cargo builds into, made absolute under `root`.
fn target_base(root: &Path) -> PathBuf {
    let configured = std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from);
    match configured {
        Some(path) if path.is_absolute() => path,
        Some(path) => root.join(path),
        None => root.join("target"),
    }
}

/// One group of lanes that run in order in one target directory.
struct Group<'table> {
    /// `gate-<group>` for a named group, `lane-<id>` for a lane on its own.
    key: String,
    /// The lanes, in the table's order.
    lanes: Vec<&'table Lane>,
}

/// Groups `selected` by feature set, cargo groups first: they are the long poles.
fn group_lanes<'table>(selected: &[&'table Lane]) -> Vec<Group<'table>> {
    let mut groups: Vec<Group<'table>> = Vec::new();
    for lane in selected {
        let key = match lane.group.as_deref() {
            Some(name) => format!("gate-{name}"),
            None => format!("lane-{}", lane.id),
        };
        match groups.iter_mut().find(|group| group.key == key) {
            Some(group) => group.lanes.push(lane),
            None => groups.push(Group {
                key,
                lanes: vec![lane],
            }),
        }
    }
    groups.sort_by_key(|group| !group.key.starts_with("gate-"));
    groups
}

/// The pieces a parallel run shares between its workers.
struct Shared<'run> {
    /// The repository root.
    root: &'run Path,
    /// The scratch directory for `TMPDIR`.
    scratch: Option<&'run Path>,
    /// The base of the per-group target directories.
    base: &'run Path,
    /// The children the gate owns.
    children: &'run Children,
    /// Where lines are printed.
    console: &'run Console,
    /// The groups still to run.
    queue: Mutex<VecDeque<Group<'run>>>,
    /// Every finished lane.
    results: Mutex<Vec<LaneResult>>,
}

/// Runs one group's lanes in order.
fn run_group(shared: &Shared<'_>, group: &Group<'_>) {
    let named = group.key.starts_with("gate-");
    let target = shared.base.join(&group.key);
    let env = child_env(shared.scratch, if named { Some(target.as_path()) } else { None });
    for lane in &group.lanes {
        if shared.children.is_stopping() {
            return;
        }
        let tag = if named { format!("  [{}]", group.key) } else { String::new() };
        shared.console.say(&format!("==> {}{tag}", lane.id));
        let log = shared.base.join("gate-logs").join(format!("{}.log", lane.id));
        let result = run_lane(lane, shared.root, &env, Some(&log), shared.children);
        report(shared.console, &result, Some(&log));
        match shared.results.lock() {
            Ok(mut held) => held.push(result),
            Err(poisoned) => poisoned.into_inner().push(result),
        }
    }
}

/// Runs lane groups concurrently; lanes inside one group run in order.
fn run_parallel(
    selected: &[&Lane],
    root: &Path,
    scratch: Option<&Path>,
    jobs: usize,
    children: &Children,
    console: &Console,
) -> Vec<LaneResult> {
    let groups = group_lanes(selected);
    let workers = jobs.max(1).min(groups.len().max(1));
    let base = target_base(root);
    let shared = Shared {
        root,
        scratch,
        base: &base,
        children,
        console,
        queue: Mutex::new(groups.into()),
        results: Mutex::new(Vec::new()),
    };
    thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let next = match shared.queue.lock() {
                        Ok(mut held) => held.pop_front(),
                        Err(poisoned) => poisoned.into_inner().pop_front(),
                    };
                    match next {
                        Some(group) => run_group(&shared, &group),
                        None => return,
                    }
                }
            });
        }
    });
    let mut finished = match shared.results.into_inner() {
        Ok(found) => found,
        Err(poisoned) => poisoned.into_inner(),
    };
    let mut ordered = Vec::new();
    for lane in selected {
        if let Some(at) = finished.iter().position(|result| result.lane_id == lane.id) {
            ordered.push(finished.swap_remove(at));
        }
    }
    ordered
}

/// What the command line asked for.
struct Options {
    /// The repository root.
    root: PathBuf,
    /// Lane ids to run.
    lanes: Vec<String>,
    /// Only the `fast` lanes.
    fast: bool,
    /// Print lane ids and exit.
    list: bool,
    /// Write a receipt.
    receipt: bool,
    /// How many groups run at once; `None` for all.
    jobs: Option<usize>,
}

/// The value after a flag.
fn flag_value<'argument>(remaining: &mut impl Iterator<Item = &'argument String>, flag: &str) -> Result<String, GateError> {
    match remaining.next() {
        Some(found) => Ok(found.clone()),
        None => Err(GateError::Usage(format!("ci-local: {flag} needs a value"))),
    }
}

/// Reads the command line.
fn parse_options(arguments: &[String]) -> Result<Options, GateError> {
    let mut options = Options {
        root: PathBuf::from("."),
        lanes: Vec::new(),
        fast: false,
        list: false,
        receipt: false,
        jobs: None,
    };
    let mut remaining = arguments.iter();
    while let Some(argument) = remaining.next() {
        match argument.as_str() {
            "--fast" => options.fast = true,
            "--list" => options.list = true,
            "--receipt" => options.receipt = true,
            "--root" => options.root = PathBuf::from(flag_value(&mut remaining, "--root")?),
            "--lane" => options.lanes.push(flag_value(&mut remaining, "--lane")?),
            "--jobs" => {
                let text = flag_value(&mut remaining, "--jobs")?;
                match text.parse::<usize>() {
                    Ok(count) => options.jobs = Some(count),
                    Err(_) => return Err(GateError::Usage(format!("ci-local: --jobs wants a number, got {text:?}"))),
                }
            }
            other => return Err(GateError::Usage(format!("ci-local: unknown argument {other:?}"))),
        }
    }
    Ok(options)
}

/// Whether a lane belongs to the local surface.
fn is_local(lane: &&Lane) -> bool {
    lane.surfaces.iter().any(|name| name == "local")
}

/// The lanes the options select.
fn select<'table>(manifest: &'table Manifest, options: &Options) -> Result<Vec<&'table Lane>, GateError> {
    if !options.lanes.is_empty() {
        let mut unknown: Vec<&str> = options
            .lanes
            .iter()
            .filter(|wanted| !manifest.lanes.iter().any(|lane| &lane.id == *wanted))
            .map(String::as_str)
            .collect();
        unknown.sort_unstable();
        if !unknown.is_empty() {
            return Err(GateError::Usage(format!("ci-local: unknown lane id(s): {}", unknown.join(", "))));
        }
        return Ok(manifest.lanes.iter().filter(|lane| options.lanes.contains(&lane.id)).collect());
    }
    if options.fast {
        return Ok(manifest.lanes.iter().filter(|lane| lane.fast).filter(is_local).collect());
    }
    Ok(manifest.lanes.iter().filter(is_local).collect())
}

/// The mode line of the header and receipt.
fn mode_of(options: &Options) -> String {
    if options.fast {
        "fast".to_owned()
    } else if options.lanes.is_empty() {
        "full".to_owned()
    } else {
        format!("lane={}", options.lanes.join(","))
    }
}

/// Prints what the run is about to do.
fn print_header(console: &Console, root: &Path, identity: &receipt::TreeIdentity, mode: &str) {
    console.say("logicalworks-crates local gate");
    console.say(&format!("  root:    {}", root.display()));
    console.say(&format!("  commit:  {}", identity.commit));
    if identity.verified() {
        console.say("  tree:    clean");
    } else {
        console.say(&format!(
            "  tree:    UNVERIFIED ({} changed, {} untracked)",
            identity.dirty.len(),
            identity.untracked.len()
        ));
    }
    console.say(&format!("  host:    {}", host()));
    console.say(&format!("  rustc:   {}", capture_line(&["rustc", "--version"], root)));
    console.say(&format!("  mode:    {mode}"));
    console.say(&format!("  started: {}", stamp()));
}

/// Runs the selected lanes and returns their results.
fn run_selected(
    selected: &[&Lane],
    options: &Options,
    root: &Path,
    console: &Console,
) -> Vec<LaneResult> {
    let parallel = selected.len() > 1 && options.jobs != Some(1);
    let children = Children::new(true);
    let scratch = Scratch::open(selected.iter().any(|lane| lane.group.is_some()));
    let shown = match scratch.path() {
        Some(path) => path.display().to_string(),
        None => "system temporary directory".to_owned(),
    };
    console.say(&format!("  scratch: {shown}"));
    console.say(&format!("  mode:    {}", if parallel { "parallel groups" } else { "one lane at a time" }));
    let results = if parallel {
        let jobs = match options.jobs {
            Some(count) if count > 0 => count,
            Some(_) | None => selected.len(),
        };
        run_parallel(selected, root, scratch.path(), jobs, &children, console)
    } else {
        let env = child_env(scratch.path(), None);
        let mut done = Vec::new();
        for lane in selected {
            console.say(&format!("\n==> {}", lane.id));
            let result = run_lane(lane, root, &env, None, &children);
            report(console, &result, None);
            done.push(result);
        }
        done
    };
    // Children first, then the scratch volume they write into.
    children.stop_all();
    children.finish();
    results
}

/// Writes the receipt, returning the exit code on failure.
fn write_receipt(
    console: &Console,
    root: &Path,
    manifest: &Manifest,
    results: &[LaneResult],
    identity: &receipt::TreeIdentity,
    mode: &str,
) -> Result<(), u8> {
    let stem: String = if identity.commit == "unknown" {
        "unknown".to_owned()
    } else {
        identity.commit.chars().take(12).collect()
    };
    let directory = root.join("evidence");
    let path = directory.join(format!("ci-local-{stem}.md"));
    let text = build_receipt(root, manifest, results, identity, mode);
    match fs::create_dir_all(&directory).and_then(|()| fs::write(&path, text)) {
        Ok(()) => {
            console.say(&format!("receipt: {}", path.display()));
            Ok(())
        }
        Err(error) => {
            console.warn(&format!("ci-local: the receipt could not be written: {error}"));
            Err(2)
        }
    }
}

/// Runs the gate with `arguments` and reports to `console`; the code is the exit status.
pub(crate) fn run(arguments: &[String], console: &Console) -> u8 {
    let options = match parse_options(arguments) {
        Ok(options) => options,
        Err(error) => {
            console.warn(&error.to_string());
            return 2;
        }
    };
    let root = match fs::canonicalize(&options.root) {
        Ok(found) => found,
        Err(error) => {
            console.warn(&format!("ci-local: {}: {error}", options.root.display()));
            return 2;
        }
    };
    let manifest = match load_lanes(&root) {
        Ok(manifest) => manifest,
        Err(error) => {
            console.warn(&format!("ci-local: cannot load {}: {error}", lanes::MANIFEST_REL));
            return 2;
        }
    };
    if options.list {
        for lane in &manifest.lanes {
            let kind = if lane.required { "required" } else { "optional" };
            console.say(&format!("{:<22} {:<9} {kind}", lane.id, lane.surfaces.join(" ")));
        }
        return 0;
    }
    let selected = match select(&manifest, &options) {
        Ok(selected) => selected,
        Err(error) => {
            console.warn(&error.to_string());
            return 2;
        }
    };
    if selected.is_empty() {
        console.warn("ci-local: no lanes selected");
        return 2;
    }
    let identity = tree_identity(&root);
    let mode = mode_of(&options);
    print_header(console, &root, &identity, &mode);
    let wall = Instant::now();
    let results = run_selected(&selected, &options, &root, console);
    console.say(&format_table(&results));
    console.say(&format!("wall: {:.1} s", wall.elapsed().as_secs_f64()));
    if options.receipt
        && let Err(code) = write_receipt(console, &root, &manifest, &results, &identity, &mode)
    {
        return code;
    }
    let refused: Vec<&LaneResult> = results
        .iter()
        .filter(|result| matches!(result.status, Status::Fail | Status::Blocked))
        .collect();
    if refused.is_empty() {
        return 0;
    }
    console.warn("\nrefused lanes:");
    for result in refused {
        match result.detail.lines().next() {
            Some(line) => console.warn(&format!("  - {}: {} — {line}", result.lane_id, result.status.label())),
            None => console.warn(&format!("  - {}: {}", result.lane_id, result.status.label())),
        }
    }
    1
}

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let console = Console::new(Box::new(io::stdout()), Box::new(io::stderr()));
    ExitCode::from(run(&arguments, &console))
}
