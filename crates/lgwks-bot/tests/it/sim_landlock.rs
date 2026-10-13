//! Simulation family: a Landlock allowlist confines a supervised child and
//! every descendant on Linux, and refuses before the fork everywhere it cannot
//! (#337, #263).
//!
//! Source: the Linux Landlock ABI (Documentation/userspace-api/landlock.rst):
//! a ruleset is deny-by-default for the access rights it handles and is
//! inherited across `fork` and `execve`, so a child cannot shed it, `setsid`
//! included. logical_ci#84 is the incident: a lane read every tenant's signing
//! key from the coordinator's tree.
//!
//! | Family | Property it pins |
//! |---|---|
//! | `shape` | validation agrees with an independent model of the documented rule, on every target |
//! | `grants` (Linux) | a child reads exactly the files under its granted directories, and a `setsid` grandchild no more |
//! | `tenants` (Linux) | concurrent tenants with distinct grants never see each other's files |
//! | refusals | an invalid, missing-path or foreign-platform allowlist starts nothing |

#![cfg(all(
    unix,
    feature = "rt",
    feature = "time",
    feature = "sync",
    feature = "process"
))]

use crate::sim;

use std::error::Error;
use std::path::Path;
#[cfg(target_os = "linux")]
use std::path::PathBuf;

use lgwks_bot::Runtime;
use lgwks_bot::rt::process::{
    Confinement, LandlockRules, PathAccess, ProcessRunError, ProcessSpec,
};
use lgwks_bot::rt::supervise::Supervisor;

type TestResult = Result<(), Box<dyn Error>>;

/// The seed space the shape family sweeps, cut into bands.
const SEEDS: u64 = 256;
const PARTS: u64 = 4;

/// One drawn index below `len`.
fn draw(sim: &mut sim::Sim, len: usize) -> Result<usize, Box<dyn Error>> {
    let bound = u32::try_from(len)?;
    Ok(usize::try_from(sim.rng().below(bound))?)
}

/// The model's verdict on one drawn rule list, restated from the documented
/// rule rather than read from the implementation.
fn model_valid(paths: &[String]) -> bool {
    (1..=LandlockRules::MAX_RULES).contains(&paths.len())
        && paths.iter().all(|path| {
            path.starts_with('/')
                && path.len() <= LandlockRules::MAX_PATH_BYTES
                && !path.contains(['\0', '\n'])
        })
}

/// One drawn path: usually absolute, sometimes hostile.
fn draw_path(sim: &mut sim::Sim) -> Result<String, Box<dyn Error>> {
    Ok(match draw(sim, 8)? {
        0 => "relative/dir".to_owned(),
        1 => String::new(),
        2 => "/has\0nul".to_owned(),
        3 => "/has\nnewline".to_owned(),
        4 => format!("/{}", "a".repeat(LandlockRules::MAX_PATH_BYTES)),
        _ => format!("/srv/tenant-{}", draw(sim, 1_000)?),
    })
}

/// One seed of the shape family: draw a rule list, check the accessor round
/// trip and the validation verdict against the model.
fn shape_case(sim: &mut sim::Sim) -> TestResult {
    let count = if draw(sim, 16)? == 0 {
        LandlockRules::MAX_RULES.saturating_add(draw(sim, 4)?)
    } else {
        draw(sim, 6)?
    };
    let mut rules = LandlockRules::new();
    let mut drawn = Vec::with_capacity(count);
    for _ in 0..count {
        let path = draw_path(sim)?;
        let access = if draw(sim, 2)? == 0 {
            PathAccess::Read
        } else {
            PathAccess::ReadWrite
        };
        rules.allow(&path, access);
        sim.trace.record(&format!("{access:?} {}", path.len()));
        drawn.push((path, access));
    }
    let read_back: Vec<(String, PathAccess)> = rules
        .rules()
        .map(|(path, access)| (path.to_string_lossy().into_owned(), access))
        .collect();
    assert_eq!(
        read_back, drawn,
        "the accessor reports every grant, in order"
    );
    let paths: Vec<String> = drawn.into_iter().map(|(path, _)| path).collect();
    let valid = model_valid(&paths);
    assert_eq!(
        rules.validate().is_ok(),
        valid,
        "validation agrees with the model on {} rules",
        paths.len()
    );
    sim.record(if valid { "valid" } else { "refused" });
    Ok(())
}

macro_rules! shape_family {
    ($($name:ident => $index:expr);+ $(;)?) => {
        $(
            /// A seeded sweep of rule lists against the validation model.
            #[test]
            fn $name() -> TestResult {
                sim::assert_replays(sim::bands(SEEDS, PARTS).swap_remove($index), shape_case)
            }
        )+
    };
}

shape_family!(
    shape_band_00 => 0; shape_band_01 => 1; shape_band_02 => 2; shape_band_03 => 3;
);

/// Run `spec` and return its refusal, failing if a child ran.
fn refusal(spec: &ProcessSpec) -> Result<std::io::ErrorKind, Box<dyn Error>> {
    let runtime = Runtime::new()?;
    let mut supervisor = Supervisor::new(1);
    match runtime.block_on(supervisor.run_process(spec)) {
        Err(ProcessRunError::NotStarted { source }) => Ok(source.kind()),
        Err(other) => Err(format!("a pre-fork refusal must be NotStarted: {other}").into()),
        Ok(run) => Err(format!("a refused allowlist ran a child: {:?}", run.exit_code()).into()),
    }
}

/// A spec that would leave `marker` behind if it ran.
fn marker_spec(marker: &Path, rules: LandlockRules) -> ProcessSpec {
    let mut spec = ProcessSpec::new("/bin/sh");
    spec.arg("-c").arg(format!("touch {}", marker.display()));
    spec.confinement(Confinement::Landlock(rules));
    spec
}

#[test]
fn an_empty_allowlist_starts_nothing() -> TestResult {
    let dir = crate::scratch::Scratch::new("landlock-empty")?;
    let marker = dir.path().join("would-run");
    let kind = refusal(&marker_spec(&marker, LandlockRules::new()))?;
    assert_eq!(kind, std::io::ErrorKind::InvalidInput);
    assert!(!marker.exists(), "the refused child never ran");
    Ok(())
}

#[test]
#[cfg(not(target_os = "linux"))]
fn an_allowlist_off_linux_starts_nothing() -> TestResult {
    let dir = crate::scratch::Scratch::new("landlock-foreign")?;
    let marker = dir.path().join("would-run");
    let mut rules = LandlockRules::new();
    rules.allow("/", PathAccess::ReadWrite);
    let kind = refusal(&marker_spec(&marker, rules))?;
    assert_eq!(kind, std::io::ErrorKind::Unsupported);
    assert!(
        !marker.exists(),
        "an unconfinable child is never run unconfined"
    );
    Ok(())
}

/// The system directories a shell and `cat` need to load, granted read-only.
#[cfg(target_os = "linux")]
fn system_rules() -> LandlockRules {
    let mut rules = LandlockRules::new();
    for dir in ["/usr", "/bin", "/lib", "/lib64", "/etc"] {
        if Path::new(dir).exists() {
            rules.allow(dir, PathAccess::Read);
        }
    }
    rules
}

#[test]
#[cfg(target_os = "linux")]
fn a_missing_path_starts_nothing() -> TestResult {
    let dir = crate::scratch::Scratch::new("landlock-missing")?;
    let marker = dir.path().join("would-run");
    let mut rules = system_rules();
    rules.allow(dir.path().join("absent"), PathAccess::ReadWrite);
    let kind = refusal(&marker_spec(&marker, rules))?;
    assert_eq!(kind, std::io::ErrorKind::NotFound);
    assert!(!marker.exists(), "the refused child never ran");
    Ok(())
}

/// Directories holding one secret each, under a scratch root.
#[cfg(target_os = "linux")]
struct Tree {
    root: crate::scratch::Scratch,
    dirs: Vec<PathBuf>,
}

#[cfg(target_os = "linux")]
impl Tree {
    fn new(name: &str, dirs: usize) -> Result<Self, Box<dyn Error>> {
        let root = crate::scratch::Scratch::new(name)?;
        let mut made = Vec::with_capacity(dirs);
        for index in 0..dirs {
            let dir = root.path().join(format!("d{index}"));
            std::fs::create_dir_all(&dir)?;
            std::fs::write(dir.join("secret"), format!("s{index}"))?;
            made.push(dir);
        }
        Ok(Self { root, dirs: made })
    }

    /// A fresh work directory under the root, one per reader.
    fn work(&self, name: &str) -> Result<PathBuf, Box<dyn Error>> {
        let work = self.root.path().join(name);
        std::fs::create_dir_all(&work)?;
        Ok(work)
    }
}

/// A script reading every secret directly and from a `setsid -w` grandchild,
/// printing `<index> <direct> <grandchild>` with 1 for a successful read, then
/// writing `work/out`.
#[cfg(target_os = "linux")]
fn reader_script(dirs: &[PathBuf], work: &Path) -> Result<String, std::fmt::Error> {
    use std::fmt::Write as _;
    let mut script = String::new();
    for (index, dir) in dirs.iter().enumerate() {
        write!(
            script,
            "d=0; cat {s} >/dev/null 2>&1 && d=1; g=0; setsid -w sh -c 'cat {s}' >/dev/null 2>&1 && g=1; echo {index} $d $g; ",
            s = dir.join("secret").display()
        )?;
    }
    write!(script, "echo ok > {}/out", work.display())?;
    Ok(script)
}

/// Run one confined reader over `dirs` with the `granted` ones readable and
/// `work` writable; return the per-directory `(direct, grandchild)` reads.
#[cfg(target_os = "linux")]
async fn confined_reads(
    dirs: Vec<PathBuf>,
    work: PathBuf,
    granted: Vec<bool>,
) -> Result<Vec<(bool, bool)>, String> {
    let mut rules = system_rules();
    // `/dev/null` is the reader's sink; `/dev` holds no tenant file.
    rules.allow("/dev", PathAccess::ReadWrite);
    rules.allow(&work, PathAccess::ReadWrite);
    for (dir, &grant) in dirs.iter().zip(&granted) {
        if grant {
            rules.allow(dir, PathAccess::Read);
        }
    }
    let mut spec = ProcessSpec::new("/bin/sh");
    spec.arg("-c")
        .arg(reader_script(&dirs, &work).map_err(|error| error.to_string())?);
    spec.capture_stdout(std::num::NonZeroUsize::new(1 << 16).ok_or("non-zero")?);
    spec.confinement(Confinement::Landlock(rules));
    let mut supervisor = Supervisor::new(1);
    let run = supervisor
        .run_process(&spec)
        .await
        .map_err(|error| error.to_string())?;
    if run.exit_code() != Some(0) {
        lgwks_std::trace::warn!(code = ?run.exit_code(), "a confined reader failed");
        return Err(format!("the confined reader exited {:?}", run.exit_code()));
    }
    if !work.join("out").exists() {
        lgwks_std::trace::warn!(
            ?work,
            "a confined reader could not write its work directory"
        );
        return Err("a ReadWrite grant must let the child write its work directory".to_owned());
    }
    let text = std::str::from_utf8(run.stdout().bytes()).map_err(|error| error.to_string())?;
    Ok(text
        .lines()
        .map(|line| {
            let mut words = line.split(' ').skip(1);
            (words.next() == Some("1"), words.next() == Some("1"))
        })
        .collect())
}

/// One seed of the grants family: a random grant set over eight directories
/// decides every read, for the child and its `setsid` grandchild alike.
#[cfg(target_os = "linux")]
fn grants_case(sim: &mut sim::Sim) -> TestResult {
    let tree = Tree::new("landlock-grants", 8)?;
    let granted: Vec<bool> = (0..8).map(|_| sim.rng().below(2) == 0).collect();
    let work = tree.work("work")?;
    let runtime = Runtime::new()?;
    let reads = runtime.block_on(confined_reads(tree.dirs.clone(), work, granted.clone()))?;
    assert_eq!(reads.len(), granted.len(), "one line per directory");
    for (index, (&grant, &(direct, grandchild))) in granted.iter().zip(&reads).enumerate() {
        assert_eq!(
            direct, grant,
            "the child reads d{index} exactly when granted"
        );
        assert_eq!(
            grandchild, grant,
            "a setsid grandchild inherits the same boundary at d{index}"
        );
        sim.trace.record(&format!("{index} {grant}"));
    }
    sim.record("decided");
    Ok(())
}

#[test]
#[cfg(target_os = "linux")]
fn grants_band_00() -> TestResult {
    sim::assert_replays(sim::bands(32, 2).swap_remove(0), grants_case)
}

#[test]
#[cfg(target_os = "linux")]
fn grants_band_01() -> TestResult {
    sim::assert_replays(sim::bands(32, 2).swap_remove(1), grants_case)
}

/// Tenants, each granted only its own directory, run concurrently: every
/// tenant (and its grandchild) reads its own secret and no other.
#[cfg(target_os = "linux")]
fn tenants_at(tenants: usize) -> TestResult {
    let tree = Tree::new("landlock-tenants", tenants)?;
    let runtime = Runtime::new()?;
    let mut works = Vec::with_capacity(tenants);
    for tenant in 0..tenants {
        works.push(tree.work(&format!("work-{tenant}"))?);
    }
    let outcomes = runtime.block_on(async {
        let mut set = lgwks_bot::rt::task::JoinSet::new();
        for (tenant, work) in works.into_iter().enumerate() {
            let granted = (0..tenants).map(|other| other == tenant).collect();
            let dirs = tree.dirs.clone();
            set.spawn(async move { (tenant, confined_reads(dirs, work, granted).await) });
        }
        let mut outcomes = Vec::with_capacity(tenants);
        while let Some(joined) = set.join_next().await {
            outcomes.push(joined?);
        }
        Ok::<_, Box<dyn Error>>(outcomes)
    })?;
    assert_eq!(outcomes.len(), tenants, "every tenant reported");
    for (tenant, reads) in outcomes {
        let reads = reads?;
        assert_eq!(
            reads.len(),
            tenants,
            "tenant {tenant} reported every directory"
        );
        for (other, &(direct, grandchild)) in reads.iter().enumerate() {
            assert_eq!(
                direct,
                other == tenant,
                "tenant {tenant} reads d{other} only if it is its own"
            );
            assert_eq!(
                grandchild,
                other == tenant,
                "tenant {tenant}'s grandchild likewise at d{other}"
            );
        }
    }
    Ok(())
}

#[test]
#[cfg(target_os = "linux")]
fn sixteen_tenants_never_read_each_other() -> TestResult {
    tenants_at(16)
}

#[test]
#[cfg(target_os = "linux")]
fn a_hundred_tenants_never_read_each_other() -> TestResult {
    tenants_at(100)
}

/// Each bound is inclusive: exactly the maximum is accepted and one past it is
/// refused, for the rule count, the path length and the profile source.
#[test]
fn every_bound_admits_its_maximum_and_refuses_one_past_it() -> TestResult {
    use lgwks_bot::rt::process::SandboxProfile;

    let mut rules = LandlockRules::new();
    for _ in 0..LandlockRules::MAX_RULES {
        rules.allow("/srv", PathAccess::Read);
    }
    rules.validate()?;
    rules.allow("/srv", PathAccess::Read);
    assert!(
        rules.validate().is_err(),
        "one rule past the maximum is refused"
    );

    let longest = format!(
        "/{}",
        "a".repeat(LandlockRules::MAX_PATH_BYTES.saturating_sub(1))
    );
    let mut at = LandlockRules::new();
    at.allow(&longest, PathAccess::Read);
    at.validate()?;
    let mut past = LandlockRules::new();
    past.allow(format!("{longest}a"), PathAccess::Read);
    assert!(
        past.validate().is_err(),
        "one byte past the longest path is refused"
    );

    let source = "a".repeat(SandboxProfile::MAX_SOURCE_BYTES);
    SandboxProfile::new(source.clone()).validate()?;
    assert!(
        SandboxProfile::new(format!("{source}a"))
            .validate()
            .is_err(),
        "one byte past the longest profile is refused"
    );
    Ok(())
}

/// An unconfined child receives exactly its own arguments: no wrapper operand
/// is prepended when nothing wraps the program.
#[test]
fn an_unconfined_child_receives_exactly_its_own_arguments() -> TestResult {
    let mut spec = ProcessSpec::new("/bin/echo");
    spec.arg("alpha").arg("beta");
    spec.capture_stdout(std::num::NonZeroUsize::new(1 << 10).ok_or("non-zero")?);
    let runtime = Runtime::new()?;
    let mut supervisor = Supervisor::new(1);
    let run = runtime.block_on(supervisor.run_process(&spec))?;
    assert_eq!(run.exit_code(), Some(0));
    assert_eq!(std::str::from_utf8(run.stdout().bytes())?, "alpha beta\n");
    Ok(())
}
