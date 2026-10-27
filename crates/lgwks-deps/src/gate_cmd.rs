//! `lgwks-deps gate`: the one installable step that makes the `script!` path
//! bind in a consumer repository (#656).
//!
//! The checks exist as separate pieces a consumer would otherwise have to
//! assemble: `check` for the dependency register, `lgwks-ast script check`
//! for the refusals without compiling, `cargo clippy` for the `forbid` lints
//! the generated flows carry, and `lgwks-ast script diff` for the
//! architecture change against the base. This command runs them in that
//! order, prints one summary and exits once: 0 every applicable leg passed, 1
//! a leg failed, 2 the gate itself could not run. `--json` emits one document
//! for an agent instead of the human transcript.
//!
//! A leg that cannot run is `skipped`, named with its reason, and never
//! counted as passed: no `lgwks-ast` on `PATH` skips the two script legs
//! (install `lgwks_ast` to run them), and no base revision skips the
//! architecture diff. Skips are visible in both renderings, so a green gate
//! with a skipped leg reads as what it is.
//!
//! Usage: `lgwks-deps gate [PATH] [--json] [--ast BIN] [--base REF|DIR]`.
//! `PATH` defaults to the working directory. `--ast` names the `lgwks-ast`
//! binary (default: `lgwks-ast` on `PATH`). `--base` names the revision or
//! directory the architecture diff reads as the base (default: `origin/main`
//! through the repository's git).

#[path = "bin/shared/children.rs"]
mod children;

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use children::{Captured, Children, Launch, STOPPED, TIMED_OUT, UNRUNNABLE};

/// How long the clippy leg may run: a consumer workspace builds real code.
const CLIPPY_BOUND: Duration = Duration::from_secs(600);

/// How many trailing lines of the clippy log the gate keeps: clippy prints
/// its errors last, so the tail is the diagnosis and the head is buildup a
/// second run reproduces.
const CLIPPY_TAIL_LINES: usize = 200;

/// How long a script-tool leg may run: it reads source, it never builds.
const SCRIPT_BOUND: Duration = Duration::from_secs(120);

/// How long one git leg may run.
const GIT_BOUND: Duration = Duration::from_secs(60);

/// Whether a leg passed, failed, could not run, or had nothing to run on.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Verdict {
    /// The leg ran and passed.
    Pass,
    /// The leg ran and refused its subject.
    Fail,
    /// The leg had no tool or no base to run on, named with its reason.
    Skip,
    /// The leg could not run to a verdict.
    Error,
}

impl Verdict {
    /// The word both renderings use.
    const fn word(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Skip => "skip",
            Self::Error => "error",
        }
    }
}

/// One leg's outcome: its verdict, the one line that says why, and the full
/// transcript the human rendering prints for a leg that did not pass.
struct Leg {
    /// The leg's name.
    name: &'static str,
    /// What it came to.
    verdict: Verdict,
    /// One line: the refusal, the skip reason, or the pass note.
    detail: String,
    /// Everything the leg printed, for the human rendering.
    transcript: String,
}

impl Leg {
    /// A leg decided without running anything.
    fn decided(name: &'static str, verdict: Verdict, detail: String) -> Self {
        Self {
            name,
            verdict,
            detail: clip(&detail, 160),
            transcript: detail,
        }
    }

    /// A leg decided by a captured child: the summary is its last
    /// interesting line, the transcript is everything it printed.
    fn captured(name: &'static str, verdict: Verdict, ran: &Captured) -> Self {
        Self {
            name,
            verdict,
            detail: summarize(ran),
            transcript: transcript_of(ran),
        }
    }

    /// A leg decided by two in-memory buffers.
    fn buffered(name: &'static str, verdict: Verdict, out: &[u8], err: &[u8]) -> Self {
        Self::captured(
            name,
            verdict,
            &Captured {
                code: 0,
                stdout: String::from_utf8_lossy(out).into_owned(),
                stderr: String::from_utf8_lossy(err).into_owned(),
            },
        )
    }
}

/// What the gate was asked to do, after one consuming pass over its arguments.
struct GateArgs {
    /// The repository to gate; the working directory when absent.
    target: PathBuf,
    /// Emit one JSON document instead of the human transcript.
    json: bool,
    /// The `lgwks-ast` binary.
    ast: String,
    /// The architecture diff's base: a revision or a directory.
    base: String,
}

/// Why the gate's arguments were refused.
struct GateArgError(String);

/// Logs `reason` where the gate refused its arguments, then hands it back:
/// a refusal the operator cannot diagnose is a refusal the operator cannot
/// repair, so every one carries the token that caused it.
fn refuse(reason: String) -> Result<GateArgs, GateArgError> {
    lgwks_std::trace::debug!(reason = %reason, "gate: refusing its arguments");
    Err(GateArgError(reason))
}

/// Parses the gate's arguments: at most one positional path, `--json` as a
/// mode that may repeat, `--ast` and `--base` each at most once. A refusal
/// names the token, the way `check`'s refusals do.
fn parse_gate_args(args: &[String]) -> Result<GateArgs, GateArgError> {
    let mut target: Option<PathBuf> = None;
    let mut json = false;
    let mut ast: Option<String> = None;
    let mut base: Option<String> = None;
    let mut cursor = args.iter();
    while let Some(argument) = cursor.next() {
        match argument.as_str() {
            "--help" | "-h" => {
                return refuse("help".to_owned());
            }
            "--json" => json = true,
            "--ast" | "--base" => {
                let flag = argument.clone();
                let Some(value) = cursor.next() else {
                    return refuse(format!("{flag} needs a value: {flag} VALUE"));
                };
                if value.starts_with("--") {
                    return refuse(format!(
                        "{flag} was given {value:?}, which is another option, not a value"
                    ));
                }
                let taken = if flag == "--ast" {
                    ast.is_some()
                } else {
                    base.is_some()
                };
                if taken {
                    return refuse(format!(
                        "{flag} was given more than once; the gate runs one tool against one base"
                    ));
                }
                if flag == "--ast" {
                    ast = Some(value.clone());
                } else {
                    base = Some(value.clone());
                }
            }
            flag if flag.starts_with("--") => {
                return refuse(format!("unknown option for `gate`: {flag}"));
            }
            path => {
                if target.is_some() {
                    return refuse(format!(
                        "`gate` audits one repository, and {path:?} is a second path"
                    ));
                }
                target = Some(PathBuf::from(path));
            }
        }
    }
    // Each option left unsaid takes its documented default: the current
    // directory, the installed `lgwks-ast`, and `origin/main` as the base.
    let target = match target {
        Some(target) => target,
        None => PathBuf::from("."),
    };
    let ast = match ast {
        Some(ast) => ast,
        None => "lgwks-ast".to_owned(),
    };
    let base = match base {
        Some(base) => base,
        None => "origin/main".to_owned(),
    };
    Ok(GateArgs {
        target,
        json,
        ast,
        base,
    })
}

/// The environment a leg's children see: inherited, minus what `cargo run`
/// exports about the crate it launched this gate for. Without that, a nested
/// `cargo clippy` fingerprints those values and rebuilds the world.
fn child_env() -> BTreeMap<String, String> {
    let mut env = children::inherited_env();
    children::forget_launched_crate(&mut env);
    env
}

/// Runs `argv` in `root` for at most `bound`, capturing what it prints.
fn capture(
    children: &Children,
    env: &BTreeMap<String, String>,
    root: &Path,
    argv: &[String],
    bound: Duration,
) -> Captured {
    children.capture(&Launch {
        argv,
        root,
        env,
        stdin: None,
        bound,
    })
}

/// The one-line summary of a captured child: its interesting last line, or
/// the code when it printed nothing.
fn summarize(captured: &Captured) -> String {
    match transcript_of(captured)
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
    {
        Some(line) => clip(line, 160),
        None => format!("exit {}", captured.code),
    }
}

/// Everything a captured child printed, stdout then stderr.
fn transcript_of(captured: &Captured) -> String {
    if captured.stderr.is_empty() {
        captured.stdout.clone()
    } else if captured.stdout.is_empty() {
        captured.stderr.clone()
    } else {
        format!("{}\n{}", captured.stdout, captured.stderr)
    }
}

/// The first `max` characters of `line`; a summary is bounded like every
/// other output this gate files.
fn clip(line: &str, max: usize) -> String {
    let clipped: String = line.chars().take(max).collect();
    if clipped.len() < line.len() {
        format!("{clipped}...")
    } else {
        clipped
    }
}

/// The admission leg: the same audit `check` runs, through the library, so
/// the gate and the command cannot disagree about a register.
fn leg_admission(root: &Path) -> Leg {
    let mut out = Vec::new();
    let mut err = Vec::new();
    // An `ExitCode` comparison is the verdict, not a guess: `run_check`
    // reports success only when every authored external edge is owned.
    match super::run_check(Some(root.to_path_buf()), None, false, &mut out, &mut err) {
        Ok(ExitCode::SUCCESS) => Leg::buffered("admission", Verdict::Pass, &out, &err),
        Ok(_) => Leg::buffered("admission", Verdict::Fail, &out, &err),
        Err(_) => Leg::buffered("admission", Verdict::Error, &out, &err),
    }
}

/// The doc-citation leg: the one implementation, called as a library.
///
/// Skips when the subject has no citation lock: pinning citations is adopted
/// by running `check-doc-citations --update`, and a repository that has not
/// adopted it has nothing for this leg to check.
fn leg_doc_citations(root: &Path) -> Leg {
    let options = lgwks_deps::doc_citations::Options::new(root.to_path_buf(), false, 5);
    library_leg(
        "doc-citations",
        &root.join("scripts/doc-citations.lock"),
        "no scripts/doc-citations.lock: adopt citation pinning with `check-doc-citations --update` to run this leg",
        |out, err| lgwks_deps::doc_citations::execute(&options, out, err),
    )
}

/// The requirements leg: the one implementation, called as a library.
///
/// Skips when the subject declares no requirements: a repository without a
/// `REQUIREMENTS.md` spine has nothing for this leg to hold unedited.
fn leg_requirements(root: &Path) -> Leg {
    library_leg(
        "requirements",
        &root.join("REQUIREMENTS.md"),
        "no REQUIREMENTS.md: nothing declares the normative sentences this leg pins",
        |out, err| lgwks_deps::requirements::execute(root, false, out, err),
    )
}

/// The spec-owner leg: the one implementation, called as a library.
///
/// Skips when the subject keeps no acceptance rows: ownership is a row
/// requiring an id, and a repository without the table has neither.
fn leg_spec_owners(root: &Path) -> Leg {
    library_leg(
        "spec-owners",
        &root.join("docs/acceptance/t-rows.toml"),
        "no docs/acceptance/t-rows.toml: nothing requires the spec ids this leg owns",
        |out, err| lgwks_deps::spec_owners::execute(root, out, err),
    )
}

/// A leg whose check is a library entry: skipped with `absent` when the
/// subject lacks `marker`, else exit 0 passes, any other exit fails, and an
/// entry that could not run is an error.
fn library_leg<E>(
    name: &'static str,
    marker: &Path,
    absent: &str,
    run: impl FnOnce(&mut Vec<u8>, &mut Vec<u8>) -> Result<u8, E>,
) -> Leg {
    if !marker.is_file() {
        return Leg::decided(name, Verdict::Skip, absent.to_owned());
    }
    let mut out = Vec::new();
    let mut err = Vec::new();
    let verdict = match run(&mut out, &mut err) {
        Ok(0) => Verdict::Pass,
        Ok(_) => Verdict::Fail,
        Err(_) => Verdict::Error,
    };
    Leg::buffered(name, verdict, &out, &err)
}

/// Whether the tool at `argv[0]` exists: a probe that asks nothing of the
/// subject, so a missing tool reads as missing rather than as a verdict
/// about the repository.
fn tool_present(children: &Children, env: &BTreeMap<String, String>, tool: &str) -> bool {
    // `--help` is the lightest probe with a stable success contract; a tool
    // that cannot even print its usage is not installed enough to gate with.
    let argv = vec![tool.to_owned(), "--help".to_owned()];
    let probed = capture(children, env, Path::new("."), &argv, GIT_BOUND);
    probed.code == 0
}

/// What the script legs run with: the owned children, their environment,
/// the subject and the `lgwks-ast` tool.
#[derive(Clone, Copy)]
struct Script<'run> {
    /// The children every launch is owned by.
    children: &'run Children,
    /// The environment the children see.
    env: &'run BTreeMap<String, String>,
    /// The subject repository.
    root: &'run Path,
    /// The `lgwks-ast` tool to run.
    ast: &'run str,
}

/// The skip a script leg named `name` reports when the tool is not installed,
/// or `None` when it is.
fn ast_missing(script: Script<'_>, name: &'static str) -> Option<Leg> {
    let Script {
        children, env, ast, ..
    } = script;
    (!tool_present(children, env, ast)).then(|| {
        Leg::decided(
            name,
            Verdict::Skip,
            format!("{ast} is not installed: install lgwks_ast to run the script legs"),
        )
    })
}

/// The script-check leg: what the language refuses, read without compiling.
fn leg_script_check(script: Script<'_>) -> Leg {
    if let Some(skip) = ast_missing(script, "script-check") {
        return skip;
    }
    let Script {
        children,
        env,
        root,
        ast,
    } = script;
    let argv = vec![
        ast.to_owned(),
        "script".to_owned(),
        "check".to_owned(),
        root.to_string_lossy().into_owned(),
    ];
    let ran = capture(children, env, root, &argv, SCRIPT_BOUND);
    let verdict = match ran.code {
        0 => Verdict::Pass,
        2 => Verdict::Fail,
        _ => Verdict::Error,
    };
    // Exit 2 is the tool's own refusal contract (a script is refused, or a
    // file could not be read); anything else is the leg failing to run.
    Leg::captured("script-check", verdict, &ran)
}

/// The clippy leg: the `forbid` lints the generated flows carry only bite
/// under clippy, so a runner that only builds never sees them.
///
/// Streams into a log file rather than a buffer: a workspace with
/// diagnostics prints megabytes, and the gate files the last
/// [`CLIPPY_TAIL_LINES`] lines, which is where clippy puts its errors.
fn leg_clippy(children: &Children, env: &BTreeMap<String, String>, root: &Path) -> Leg {
    let Some(log) = clippy_log(root) else {
        return Leg::decided(
            "clippy",
            Verdict::Error,
            "no log file could be claimed: the clippy leg has nowhere to stream".to_owned(),
        );
    };
    let file = match std::fs::File::create(&log) {
        Ok(file) => file,
        Err(error) => {
            return Leg::decided(
                "clippy",
                Verdict::Error,
                format!(
                    "the clippy log {} could not be written: {error}",
                    log.display()
                ),
            );
        }
    };
    let code = children.run(
        &Launch {
            argv: &[
                "cargo".to_owned(),
                "clippy".to_owned(),
                "--locked".to_owned(),
                "--all-targets".to_owned(),
                "--".to_owned(),
                "-D".to_owned(),
                "warnings".to_owned(),
            ],
            root,
            env,
            stdin: None,
            bound: CLIPPY_BOUND,
        },
        Some(&file),
    );
    drop(file);
    let transcript = clippy_tail(&log);
    if let Err(error) = std::fs::remove_file(&log) {
        lgwks_std::trace::debug!(%error, path = ?log, "the clippy log could not be removed");
    }
    if code == UNRUNNABLE {
        return Leg::decided(
            "clippy",
            Verdict::Error,
            "cargo could not be started: the gate audits Rust, and without cargo it cannot"
                .to_owned(),
        );
    }
    if code == STOPPED {
        return Leg::decided(
            "clippy",
            Verdict::Error,
            "the gate stopped before clippy ran".to_owned(),
        );
    }
    // Clippy's own exit is the verdict: the flow `forbid` lints are errors
    // under `-D warnings`, so a refusal reads as a failure, not as noise.
    let mut leg = Leg::decided(
        "clippy",
        if code == 0 {
            Verdict::Pass
        } else if code == TIMED_OUT {
            Verdict::Error
        } else {
            Verdict::Fail
        },
        summarize_transcript(&transcript),
    );
    leg.transcript = transcript;
    leg
}

/// 64 bits from the one entropy source, `lgwks_std::random`. This crate takes
/// `random` off wasm (INV-RANDOM-ONE-SOURCE), where no gate child can spawn
/// either, so there it is `None` and each caller skips with its reason.
#[cfg(not(target_family = "wasm"))]
fn gate_bits() -> Option<[u8; 8]> {
    lgwks_std::random::bytes::<8>().ok()
}

/// No entropy source on wasm: see the native `gate_bits`.
#[cfg(target_family = "wasm")]
fn gate_bits() -> Option<[u8; 8]> {
    None
}

/// The clippy log's path: 64 random bits under the subject's target
/// directory, so two gates never share one and no build output lands in the
/// repository. The directory is created: a fresh checkout has no `target`
/// until cargo makes one, and the log must exist before clippy runs.
fn clippy_log(root: &Path) -> Option<PathBuf> {
    let bits = gate_bits()?;
    let dir = root.join("target");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join(format!(
        "lgwks-gate-clippy-{:016x}.log",
        u64::from_le_bytes(bits)
    )))
}

/// The log's last [`CLIPPY_TAIL_LINES`] lines, or the code when it is empty.
fn clippy_tail(log: &Path) -> String {
    let text = match std::fs::read_to_string(log) {
        Ok(text) => text,
        Err(error) => {
            lgwks_std::trace::debug!(%error, path = ?log, "the clippy log could not be read");
            String::new()
        }
    };
    let lines: Vec<&str> = text.lines().collect();
    let tail = lines
        .iter()
        .rev()
        .take(CLIPPY_TAIL_LINES)
        .rev()
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    if tail.trim().is_empty() {
        "clippy printed nothing".to_owned()
    } else {
        tail
    }
}

/// The one-line summary of a transcript: its last interesting line.
fn summarize_transcript(transcript: &str) -> String {
    match transcript
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
    {
        Some(line) => clip(line, 160),
        None => "clippy printed nothing".to_owned(),
    }
}

/// A scratch directory no other gate run holds: 64 random bits under the
/// subject's target directory, claimed with an exclusive `create_dir`, so two
/// gates on one machine never share one. `None` when no entropy answers (then
/// the architecture leg skips with its reason rather than gating on a
/// directory it cannot claim).
fn scratch_dir(root: &Path) -> Option<PathBuf> {
    let bits = gate_bits()?;
    let candidate = root.join(format!(
        "target/lgwks-gate-base-{:016x}",
        u64::from_le_bytes(bits)
    ));
    std::fs::create_dir(&candidate).ok().map(|()| candidate)
}

/// The base revision exported beside the subject, or the skip reason when no
/// base can be produced. A directory base is used as given; a revision is
/// exported with a detached worktree, which needs only git, and removed
/// afterwards whether or not the diff ran.
fn export_base(
    children: &Children,
    env: &BTreeMap<String, String>,
    root: &Path,
    base: &str,
) -> Result<PathBuf, NoBase> {
    if Path::new(base).is_dir() {
        return Ok(PathBuf::from(base));
    }
    let sha_argv = vec![
        "git".to_owned(),
        "merge-base".to_owned(),
        "HEAD".to_owned(),
        base.to_owned(),
    ];
    let sha = capture(children, env, root, &sha_argv, GIT_BOUND);
    if sha.code != 0 {
        return no_base(format!(
            "no base: `git merge-base HEAD {base}` did not succeed — pass a directory to --base to diff against it"
        ));
    }
    let Some(revision) = sha
        .stdout
        .lines()
        .next()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    else {
        return no_base(format!(
            "no base: `git merge-base HEAD {base}` printed no revision"
        ));
    };
    let Some(dir) = scratch_dir(root) else {
        return no_base("no base: no scratch directory could be claimed".to_owned());
    };
    let add = capture(
        children,
        env,
        root,
        &[
            "git".to_owned(),
            "worktree".to_owned(),
            "add".to_owned(),
            "--detach".to_owned(),
            dir.to_string_lossy().into_owned(),
            revision.to_owned(),
        ],
        GIT_BOUND,
    );
    if add.code != 0 {
        if let Err(error) = std::fs::remove_dir_all(&dir) {
            lgwks_std::trace::debug!(%error, path = ?dir, "a failed base export left its scratch directory");
        }
        return no_base(format!(
            "no base: the {base} revision ({}) could not be checked out: {}",
            clip(revision, 40),
            summarize(&add)
        ));
    }
    Ok(dir)
}

/// Why no base could be produced: the architecture leg skips with this reason,
/// which names what to pass instead.
#[derive(Debug)]
struct NoBase {
    /// The skip reason the leg reports.
    reason: String,
}

/// Logs `reason` where the base could not be produced, then hands it back.
fn no_base(reason: String) -> Result<PathBuf, NoBase> {
    lgwks_std::trace::debug!(reason = %reason, "gate: the architecture leg has no base");
    Err(NoBase { reason })
}

/// The architecture leg: the structural change between the base and the
/// subject, failing on a widening no `// lgwks-architecture-approved:
/// <reason>` line approves.
fn leg_arch_diff(script: Script<'_>, base: &str) -> Leg {
    if let Some(skip) = ast_missing(script, "arch-diff") {
        return skip;
    }
    let Script {
        children,
        env,
        root,
        ast,
    } = script;
    let based = match export_base(children, env, root, base) {
        Ok(dir) => dir,
        Err(NoBase { reason }) => {
            return Leg::decided("arch-diff", Verdict::Skip, reason);
        }
    };
    // A directory base is the caller's, and stays; an exported worktree is
    // the gate's, and goes whatever the diff says.
    let owned = based.to_string_lossy() != base;
    let argv = vec![
        ast.to_owned(),
        "script".to_owned(),
        "diff".to_owned(),
        based.to_string_lossy().into_owned(),
        root.to_string_lossy().into_owned(),
    ];
    let ran = capture(children, env, root, &argv, SCRIPT_BOUND);
    if owned {
        let _ = capture(
            children,
            env,
            root,
            &[
                "git".to_owned(),
                "worktree".to_owned(),
                "remove".to_owned(),
                "--force".to_owned(),
                based.to_string_lossy().into_owned(),
            ],
            GIT_BOUND,
        );
    }
    // The diff tool's own contract: 0 nothing widens unapproved, 1 a
    // widening stands without its approval line, 2 the comparison itself
    // could not run.
    Leg::captured(
        "arch-diff",
        match ran.code {
            0 => Verdict::Pass,
            1 => Verdict::Fail,
            _ => Verdict::Error,
        },
        &ran,
    )
}

/// Runs every leg in order: admission, citations, requirements, owners, then
/// the script legs and clippy that need tools outside this binary.
fn run_legs(root: &Path, ast: &str, base: &str) -> Vec<Leg> {
    let children = Children::new(false);
    let env = child_env();
    let script = Script {
        children: &children,
        env: &env,
        root,
        ast,
    };
    let legs = vec![
        leg_admission(root),
        leg_doc_citations(root),
        leg_requirements(root),
        leg_spec_owners(root),
        leg_script_check(script),
        leg_clippy(&children, &env, root),
        leg_arch_diff(script, base),
    ];
    children.stop_all();
    children.finish();
    legs
}

/// The gate's exit: 0 every applicable leg passed, 1 a leg failed, 2 a leg
/// errored or the gate was misused. A skip is neither: it is named, and it is
/// not evidence.
fn gate_code(legs: &[Leg]) -> ExitCode {
    if legs.iter().any(|leg| leg.verdict == Verdict::Error) {
        ExitCode::from(2)
    } else if legs.iter().any(|leg| leg.verdict == Verdict::Fail) {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// One JSON string field: quoted, with the three escapes a detail can carry.
fn field(text: &str) -> String {
    let mut quoted = String::with_capacity(text.len().saturating_add(2));
    quoted.push('"');
    for ch in text.chars() {
        match ch {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            '\n' => quoted.push(' '),
            _ => quoted.push(ch),
        }
    }
    quoted.push('"');
    quoted
}

/// The machine rendering: one document, one leg per row.
fn render_json(root: &Path, base: &str, legs: &[Leg]) -> String {
    let rows: Vec<String> = legs
        .iter()
        .map(|leg| {
            format!(
                "{{\"name\":{},\"verdict\":{},\"detail\":{}}}",
                field(leg.name),
                field(leg.verdict.word()),
                field(&leg.detail),
            )
        })
        .collect();
    format!(
        "{{\"tool\":\"lgwks-deps-gate\",\"path\":{},\"base\":{},\"legs\":[\n{}\n]}}\n",
        field(&root.to_string_lossy()),
        field(base),
        rows.join(",\n"),
    )
}

/// The human rendering: one summary line per leg, the transcripts of the
/// legs that did not pass, then the verdict.
fn render_human(root: &Path, legs: &[Leg], out: &mut impl io::Write) -> io::Result<()> {
    for leg in legs {
        writeln!(
            out,
            "{:13} {:5}  {}",
            leg.name,
            leg.verdict.word(),
            leg.detail
        )?;
    }
    for leg in legs.iter().filter(|leg| leg.verdict != Verdict::Pass) {
        writeln!(
            out,
            "\n--- {} ({}) ---\n{}",
            leg.name,
            leg.verdict.word(),
            leg.transcript
        )?;
    }
    let failures = legs
        .iter()
        .filter(|leg| leg.verdict == Verdict::Fail)
        .count();
    let errors = legs
        .iter()
        .filter(|leg| leg.verdict == Verdict::Error)
        .count();
    let skips = legs
        .iter()
        .filter(|leg| leg.verdict == Verdict::Skip)
        .count();
    if failures == 0 && errors == 0 {
        writeln!(
            out,
            "GATE PASS  {} — every applicable leg passed{}",
            root.display(),
            if skips == 0 {
                String::new()
            } else {
                format!(", {skips} skipped (named above, not evidence)")
            }
        )
    } else {
        writeln!(
            out,
            "GATE REFUSED  {} — {failures} leg(s) failed, {errors} errored",
            root.display()
        )
    }
}

/// Handles `gate`, keeping the argument parsing out of the legs.
pub(crate) fn handle_gate(
    args: &[String],
    out: &mut impl io::Write,
    err: &mut impl io::Write,
) -> io::Result<ExitCode> {
    let options = match parse_gate_args(args) {
        Ok(options) => options,
        Err(GateArgError(reason)) => {
            if reason == "help" {
                return super::handle_help(out);
            }
            writeln!(err, "lgwks-deps: {reason}\n")?;
            write!(err, "{}", super::USAGE)?;
            return Ok(ExitCode::from(2));
        }
    };
    let legs = run_legs(&options.target, &options.ast, &options.base);
    if options.json {
        write!(
            out,
            "{}",
            render_json(&options.target, &options.base, &legs)
        )?;
    } else {
        render_human(&options.target, &legs, out)?;
    }
    Ok(gate_code(&legs))
}
