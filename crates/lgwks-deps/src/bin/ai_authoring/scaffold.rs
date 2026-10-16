//! A trial's directory: the template with its placeholders filled, the files the
//! model reads, the candidate under test and the oracle that grades it.

use std::fs;
use std::path::{Path, PathBuf};

use super::author::Texts;
use super::bench::{Bench, fill};
use super::cargo::{Built, Cargo, Tested};
use super::plan::{Job, package_for, trial_id};
use super::trial::Judge;
use super::{BenchError, create_dir, fail, read, write};

/// The model's own directory inside a trial: only the prompt files, nothing else.
const SCRATCH: &str = "scratch";

/// A trial's directory.
pub(super) struct TrialDir {
    /// The directory.
    pub(super) path: PathBuf,
    /// The directory's name, the trial's id.
    pub(super) id: String,
    /// The trial crate's package name.
    pub(super) package: String,
}

impl TrialDir {
    /// The model's scratch directory.
    pub(super) fn scratch(&self) -> PathBuf {
        self.path.join(SCRATCH)
    }
}

/// The text of a task's prompt.
pub(super) fn task_prompt(bench: &Bench, task: &str) -> Result<String, BenchError> {
    read(&bench.path(&fill(&bench.layout.prompt, &[("task", task)])))
}

/// The text of a task's oracle, as committed.
pub(super) fn oracle_text(bench: &Bench, task: &str) -> Result<String, BenchError> {
    read(&bench.path(&fill(&bench.layout.oracle, &[("task", task)])))
}

/// The text of a reference solution.
pub(super) fn reference_text(bench: &Bench, surface: &str, task: &str) -> Result<String, BenchError> {
    read(&bench.path(&fill(
        &bench.layout.reference,
        &[("surface", surface), ("task", task)],
    )))
}

/// The text of a task's mutant.
pub(super) fn mutant_text(bench: &Bench, task: &str) -> Result<String, BenchError> {
    read(&bench.path(&fill(&bench.layout.mutant, &[("task", task)])))
}

/// What a model prompt for `job` is made of.
pub(super) fn prompt_texts(bench: &Bench, job: &Job) -> Result<Texts, BenchError> {
    let Some(arm) = bench.arm(&job.arm) else {
        return fail(
            BenchError::Usage(format!("{} is not a declared arm", job.arm)),
            "prompt_texts",
        );
    };
    let preamble = match bench.profile(&job.profile) {
        Some(profile) => profile.text.clone(),
        None => String::new(),
    };
    Ok(Texts {
        preamble,
        task: task_prompt(bench, &job.task)?,
        sheet: read(&bench.path(&arm.sheet))?,
    })
}

/// `path` resolved, naming it when it cannot be.
fn resolve(path: &Path) -> Result<PathBuf, BenchError> {
    match fs::canonicalize(path) {
        Ok(found) => Ok(found),
        Err(cause) => fail(
            BenchError::Io {
                path: path.to_path_buf(),
                cause,
            },
            "resolve",
        ),
    }
}

/// Creates the trial's directory with its template filled in.
///
/// A fresh run owns a fresh work root, so a trial directory never exists
/// already; finding one means two runs collided, and nothing is overwritten.
pub(super) fn prepare(bench: &Bench, work: &Path, job: &Job) -> Result<TrialDir, BenchError> {
    let id = trial_id(job);
    let package = package_for(&bench.package_prefix, &id);
    let path = work.join(&id);
    create_dir(&path)?;
    let dir = TrialDir { path, id, package };
    create_dir(&dir.scratch())?;
    create_dir(&dir.path.join("src"))?;
    let template = bench.path(&bench.layout.template);
    let mut manifest = read(&template.join("Cargo.toml"))?.replace(&bench.template.name_token, &dir.package);
    for entry in &bench.template.paths {
        let target = resolve(&bench.path(&entry.target))?;
        manifest = manifest.replace(&entry.token, &target.display().to_string());
    }
    write(&dir.path.join("Cargo.toml"), &manifest)?;
    let name_line = |name: &str| format!("name = \"{name}\"");
    let lock = read(&bench.path(&bench.layout.lock))?.replace(
        &name_line(&bench.template.name_token),
        &name_line(&dir.package),
    );
    write(&dir.path.join("Cargo.lock"), &lock)?;
    Ok(dir)
}

/// Files a trial's model reads, written into its scratch directory.
pub(super) fn write_scratch(dir: &TrialDir, texts: &Texts) -> Result<(), BenchError> {
    let scratch = dir.scratch();
    write(&scratch.join("prompt.md"), &texts.task)?;
    write(&scratch.join("api.md"), &texts.sheet)?;
    if !texts.preamble.is_empty() {
        write(&scratch.join("role.md"), &texts.preamble)?;
    }
    Ok(())
}

/// Places the reference beside a mutant as `src/reference.rs`.
///
/// A mutant is its reference plus one mutation, never a forked copy, so the two
/// cannot drift: a mutant that stops differing from its reference in anything
/// but the intended mutation shows up as a diff rather than as a verdict.
pub(super) fn place_reference(bench: &Bench, dir: &TrialDir, job: &Job) -> Result<(), BenchError> {
    let Some(task) = bench.task(&job.task) else {
        return fail(
            BenchError::Usage(format!("{} is not a declared task", job.task)),
            "place_reference",
        );
    };
    write(
        &dir.path.join("src").join("reference.rs"),
        &reference_text(bench, &task.mutant_base, &job.task)?,
    )
}

/// Writes the files a finished trial leaves beside its crate.
pub(super) fn write_outputs(
    dir: &TrialDir,
    code: &str,
    raw: &str,
    build_log: &str,
    test_output: &str,
) -> Result<(), BenchError> {
    write(&dir.path.join("lib.rs"), code)?;
    write(&dir.path.join("raw.ndjson"), raw)?;
    write(&dir.path.join("build.log"), build_log)?;
    write(&dir.path.join("test.log"), test_output)
}

/// Compiles candidates and runs the oracle in a trial's directory, through cargo.
pub(super) struct CargoJudge<'run> {
    /// Cargo, serialized.
    pub(super) cargo: &'run Cargo<'run>,
    /// The trial's directory.
    pub(super) dir: &'run TrialDir,
    /// The oracle, with the crate under test named.
    pub(super) oracle: String,
}

impl Judge for CargoJudge<'_> {
    fn build(&self, code: &str) -> Result<Built, BenchError> {
        write(&self.dir.path.join("src").join("lib.rs"), code)?;
        self.cargo.build(&self.dir.path)
    }

    fn grade(&self) -> Result<Tested, BenchError> {
        let tests = self.dir.path.join("tests");
        create_dir(&tests)?;
        write(&tests.join("oracle.rs"), &self.oracle)?;
        self.cargo.test(&self.dir.path)
    }
}

/// The oracle's text with the crate under test named, and its clause names.
pub(super) fn oracle_for(
    bench: &Bench,
    task: &str,
    package: &str,
) -> Result<(String, Vec<String>), BenchError> {
    let committed = oracle_text(bench, task)?;
    let clauses = super::grade::oracle_test_names(&committed);
    Ok((committed.replace(&bench.oracle_token, package), clauses))
}
