//! The benchmark as it declares itself: `bench.toml` in the benchmark's own
//! directory names its arms, tasks, profiles, file layout, limits, defaults, the
//! model command line and the protocol it records, so the runner holds none of
//! them.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::toml_subset::{Table, list, raw, read_tables, text};
use super::{BenchError, fail, read};

/// The manifest's file name inside the benchmark directory.
pub(super) const MANIFEST: &str = "bench.toml";

/// Where the benchmark's files are, relative to its directory.
///
/// `prompt`, `oracle`, `reference` and `mutant` are patterns: `{task}` and
/// `{surface}` stand for the names they say.
pub(super) struct Layout {
    /// The crate template's directory.
    pub(super) template: String,
    /// The committed lock every trial copies.
    pub(super) lock: String,
    /// A task's prompt.
    pub(super) prompt: String,
    /// A task's hidden oracle.
    pub(super) oracle: String,
    /// A reference solution.
    pub(super) reference: String,
    /// A task's deliberate mutant.
    pub(super) mutant: String,
    /// Where committed runs go.
    pub(super) runs: String,
}

/// One path the template names that the runner points at a real directory.
pub(super) struct TemplatePath {
    /// The text in the template's manifest.
    pub(super) token: String,
    /// The directory it stands for, relative to the benchmark directory.
    pub(super) target: String,
}

/// How the crate template becomes a trial crate.
pub(super) struct Template {
    /// The text the template's package name is written as.
    pub(super) name_token: String,
    /// The paths to point at real directories.
    pub(super) paths: Vec<TemplatePath>,
}

/// The model command and the sandbox that keeps its author closed-book.
pub(super) struct ModelCli {
    /// The model command.
    pub(super) command: String,
    /// The sandbox program that wraps the model command.
    pub(super) sandbox: String,
    /// The model command's arguments; `{model}` stands for the model id.
    pub(super) args: Vec<String>,
    /// The system directories the author may not read, besides the home
    /// directory and the run's work root.
    pub(super) denied: Vec<String>,
    /// The model command's own settings directory, under the home directory.
    pub(super) config_dir: String,
    /// What the settings directory holds that the author may not read.
    pub(super) withheld: Vec<String>,
    /// Further directories under the home directory the command needs, by
    /// name; none when it keeps everything in its settings directory.
    pub(super) allowed: Vec<String>,
    /// The environment the command is given beyond the run's own, as
    /// `NAME=value`; `{scratch}` stands for the trial's scratch directory.
    pub(super) env: Vec<String>,
}

/// How long each kind of child may run.
pub(super) struct Limits {
    /// One model call.
    pub(super) model_call: Duration,
    /// One `cargo build`.
    pub(super) build: Duration,
    /// One oracle run.
    pub(super) oracle: Duration,
    /// One `git` query.
    pub(super) git: Duration,
    /// The most tokens, read and written, every model call of a run may bill.
    pub(super) token_budget: u64,
    /// The most tokens one reply may bill, reasoning included; a call is
    /// admitted only when this and its prompt still fit the budget.
    pub(super) call_tokens: u64,
}

/// What a run does when the command line does not say.
pub(super) struct Defaults {
    /// The models a models run asks.
    pub(super) models: Vec<String>,
    /// Trials per cell.
    pub(super) trials: u32,
    /// Trials running at once.
    pub(super) parallel: usize,
    /// Repairs a failing compile gets.
    pub(super) max_repairs: u32,
}

/// One fixed persona preamble.
pub(super) struct Profile {
    /// The profile's name.
    pub(super) name: String,
    /// The preamble prepended to the prompt skeleton.
    pub(super) text: String,
}

/// One API surface an author is given a sheet for.
pub(super) struct Arm {
    /// The arm's name.
    pub(super) name: String,
    /// The sheet the author reads, relative to the benchmark directory.
    pub(super) sheet: String,
    /// The surface whose reference solutions this arm's dry run compiles.
    pub(super) reference: String,
}

/// A reference solution one arm borrows from another surface for one task.
pub(super) struct Borrowed {
    /// The arm that borrows.
    pub(super) arm: String,
    /// The task it borrows for.
    pub(super) task: String,
    /// The surface it borrows from.
    pub(super) surface: String,
}

/// One held-out task.
pub(super) struct Task {
    /// The task's name.
    pub(super) name: String,
    /// The oracle clause that says a dropped future left nothing live.
    pub(super) drop_clause: String,
    /// The surface whose reference a mutant of this task is one mutation from.
    pub(super) mutant_base: String,
}

/// An arm and task pair the benchmark does not run, and why.
pub(super) struct Exclusion {
    /// The task.
    pub(super) task: String,
    /// The arm.
    pub(super) arm: String,
    /// Why the arm cannot express the task.
    pub(super) why: String,
}

/// A protocol statement a summary records.
pub(super) enum Setting {
    /// A sentence.
    Words(String),
    /// A yes or no.
    Flag(bool),
}

/// The benchmark.
pub(super) struct Bench {
    /// The benchmark directory.
    pub(super) root: PathBuf,
    /// The reply contract that closes every prompt.
    pub(super) framing: String,
    /// The text the committed oracles name the crate under test with.
    pub(super) oracle_token: String,
    /// The prefix of every trial crate's package name.
    pub(super) package_prefix: String,
    /// The tokens counted as author burden.
    pub(super) tokens: Vec<String>,
    /// The arm a mutant's record is filed under.
    pub(super) mutant_arm: String,
    /// What a record says about the lock its trial built from.
    pub(super) lockfile_note: String,
    /// Where the files are.
    pub(super) layout: Layout,
    /// The crate template.
    pub(super) template: Template,
    /// The model command.
    pub(super) model: ModelCli,
    /// The bounds on children.
    pub(super) limits: Limits,
    /// What a run does by default.
    pub(super) defaults: Defaults,
    /// The profiles, in order.
    pub(super) profiles: Vec<Profile>,
    /// The arms, in order.
    pub(super) arms: Vec<Arm>,
    /// The references arms borrow.
    pub(super) borrowed: Vec<Borrowed>,
    /// The tasks, in order.
    pub(super) tasks: Vec<Task>,
    /// The pairs that are not run.
    pub(super) exclusions: Vec<Exclusion>,
    /// The protocol statements a summary records.
    pub(super) protocol: Vec<(String, Setting)>,
}

/// The string under `key`, or a refusal naming the table and key.
fn required(table: &Table, key: &str) -> Result<String, BenchError> {
    match text(table, key) {
        Some(found) if !found.is_empty() => Ok(found.to_owned()),
        Some(_) | None => fail(
            BenchError::Manifest(format!("[{}] needs a non-empty {key}", table.path)),
            "required",
        ),
    }
}

/// The strings under `key`, or a refusal when there are none.
fn required_list(table: &Table, key: &str) -> Result<Vec<String>, BenchError> {
    match list(table, key) {
        Some(found) if !found.is_empty() => Ok(found.to_vec()),
        Some(_) | None => fail(
            BenchError::Manifest(format!("[{}] needs a non-empty list {key}", table.path)),
            "required_list",
        ),
    }
}

/// The strings under `key`, none when the key is absent: the manifest says
/// nothing, and the command needs nothing, so there is nothing to refuse.
fn optional_list(table: &Table, key: &str) -> Vec<String> {
    match list(table, key) {
        Some(found) => found.to_vec(),
        None => Vec::new(),
    }
}

/// The whole number under `key`, or a refusal when it is absent or is not one.
fn number(table: &Table, key: &str) -> Result<u64, BenchError> {
    match raw(table, key).and_then(|written| written.parse::<u64>().ok()) {
        Some(found) if found > 0 => Ok(found),
        Some(_) | None => fail(
            BenchError::Manifest(format!(
                "[{}] needs {key} as a whole number above zero",
                table.path
            )),
            "number",
        ),
    }
}

/// `number` as a count that fits `u32`.
fn count(table: &Table, key: &str) -> Result<u32, BenchError> {
    match u32::try_from(number(table, key)?) {
        Ok(found) => Ok(found),
        Err(_) => fail(
            BenchError::Manifest(format!("[{}] {key} does not fit 32 bits", table.path)),
            "count",
        ),
    }
}

/// The one table named `path`, or a refusal.
fn only<'tables>(tables: &'tables [Table], path: &str) -> Result<&'tables Table, BenchError> {
    match tables
        .iter()
        .find(|table| table.path == path && !table.array)
    {
        Some(found) => Ok(found),
        None => fail(
            BenchError::Manifest(format!("the manifest has no [{path}] table")),
            "only",
        ),
    }
}

/// Every `[[path]]` element.
fn elements<'tables>(tables: &'tables [Table], path: &str) -> Vec<&'tables Table> {
    tables
        .iter()
        .filter(|table| table.path == path && table.array)
        .collect()
}

/// Every `[[path]]` element, which the manifest must have at least one of.
fn each<'tables>(tables: &'tables [Table], path: &str) -> Result<Vec<&'tables Table>, BenchError> {
    let found = elements(tables, path);
    if found.is_empty() {
        return fail(
            BenchError::Manifest(format!("the manifest has no [[{path}]] element")),
            "each",
        );
    }
    Ok(found)
}

/// A refusal when a name is unsafe in a path, or occurs twice among `names`.
///
/// Names become directory names, package names and file names, so a name with a
/// separator or a dot-dot would let a manifest reach outside its work root.
fn distinct(kind: &str, names: &[&str]) -> Result<(), BenchError> {
    for (at, name) in names.iter().enumerate() {
        let safe = !name.is_empty()
            && *name != "."
            && *name != ".."
            && name
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-'));
        if !safe {
            return fail(
                BenchError::Manifest(format!(
                    "the {kind} {name:?} must be non-empty and use only [A-Za-z0-9_.-]"
                )),
                "distinct",
            );
        }
        if names
            .iter()
            .skip(at.saturating_add(1))
            .any(|other| other == name)
        {
            return fail(
                BenchError::Manifest(format!("the {kind} {name:?} is declared twice")),
                "distinct",
            );
        }
    }
    Ok(())
}

/// The statements of `[protocol]`, words and flags.
fn settings(table: &Table) -> Vec<(String, Setting)> {
    table
        .entries
        .iter()
        .filter_map(|entry| {
            let key = entry.key.clone();
            match text(table, &key) {
                Some(words) => Some((key, Setting::Words(words.to_owned()))),
                None => match raw(table, &key) {
                    Some("true") => Some((key, Setting::Flag(true))),
                    Some("false") => Some((key, Setting::Flag(false))),
                    Some(_) | None => None,
                },
            }
        })
        .collect()
}

/// `pattern` with each `{key}` replaced by its value.
pub(super) fn fill(pattern: &str, pairs: &[(&str, &str)]) -> String {
    pairs.iter().fold(pattern.to_owned(), |filled, pair| {
        filled.replace(&format!("{{{}}}", pair.0), pair.1)
    })
}

/// The `[layout]` table.
fn layout_of(table: &Table) -> Result<Layout, BenchError> {
    let layout = Layout {
        template: required(table, "template")?,
        lock: required(table, "lock")?,
        prompt: required(table, "prompt")?,
        oracle: required(table, "oracle")?,
        reference: required(table, "reference")?,
        mutant: required(table, "mutant")?,
        runs: required(table, "runs")?,
    };
    Ok(layout)
}

/// The `[model]` table.
fn model_of(table: &Table) -> Result<ModelCli, BenchError> {
    let model = ModelCli {
        command: required(table, "command")?,
        sandbox: required(table, "sandbox")?,
        args: required_list(table, "args")?,
        denied: required_list(table, "denied")?,
        config_dir: required(table, "config_dir")?,
        withheld: required_list(table, "withheld")?,
        allowed: optional_list(table, "allowed"),
        env: optional_list(table, "env"),
    };
    Ok(model)
}

impl Bench {
    /// Reads `bench.toml` in `root`.
    pub(super) fn load(root: &Path) -> Result<Self, BenchError> {
        Self::parse(root, &read(&root.join(MANIFEST))?)
    }

    /// The benchmark a manifest's text declares, rooted at `root`.
    pub(super) fn parse(root: &Path, manifest: &str) -> Result<Self, BenchError> {
        let tables = read_tables(manifest);
        let top = only(&tables, "")?;
        let template = only(&tables, "template")?;
        let limits = only(&tables, "limits")?;
        let defaults = only(&tables, "defaults")?;
        let mut paths = Vec::new();
        for table in each(&tables, "template.path")? {
            paths.push(TemplatePath {
                token: required(table, "token")?,
                target: required(table, "target")?,
            });
        }
        let mut profiles = Vec::new();
        for table in each(&tables, "profile")? {
            profiles.push(Profile {
                name: required(table, "name")?,
                text: required(table, "text")?,
            });
        }
        let mut arms = Vec::new();
        for table in each(&tables, "arm")? {
            let name = required(table, "name")?;
            let reference = match text(table, "reference") {
                Some(surface) if !surface.is_empty() => surface.to_owned(),
                Some(_) | None => name.clone(),
            };
            arms.push(Arm {
                name,
                sheet: required(table, "sheet")?,
                reference,
            });
        }
        let mut tasks = Vec::new();
        for table in each(&tables, "task")? {
            tasks.push(Task {
                name: required(table, "name")?,
                drop_clause: required(table, "drop_clause")?,
                mutant_base: required(table, "mutant_base")?,
            });
        }
        let mut borrowed = Vec::new();
        for table in elements(&tables, "borrowed") {
            borrowed.push(Borrowed {
                arm: required(table, "arm")?,
                task: required(table, "task")?,
                surface: required(table, "surface")?,
            });
        }
        let mut exclusions = Vec::new();
        for table in elements(&tables, "exclusion") {
            exclusions.push(Exclusion {
                task: required(table, "task")?,
                arm: required(table, "arm")?,
                why: required(table, "why")?,
            });
        }
        let bench = Self {
            root: root.to_path_buf(),
            framing: required(top, "framing")?,
            oracle_token: required(top, "oracle_crate_token")?,
            package_prefix: required(top, "package_prefix")?,
            tokens: required_list(top, "orchestration_tokens")?,
            mutant_arm: required(top, "mutant_arm")?,
            lockfile_note: required(top, "lockfile_note")?,
            layout: layout_of(only(&tables, "layout")?)?,
            template: Template {
                name_token: required(template, "name_token")?,
                paths,
            },
            model: model_of(only(&tables, "model")?)?,
            limits: Limits {
                model_call: Duration::from_secs(number(limits, "model_call_s")?),
                build: Duration::from_secs(number(limits, "build_s")?),
                oracle: Duration::from_secs(number(limits, "oracle_s")?),
                git: Duration::from_secs(number(limits, "git_s")?),
                token_budget: number(limits, "token_budget")?,
                call_tokens: number(limits, "call_tokens")?,
            },
            defaults: Defaults {
                models: required_list(defaults, "models")?,
                trials: count(defaults, "trials")?,
                parallel: match usize::try_from(number(defaults, "parallel")?) {
                    Ok(workers) => workers,
                    Err(_) => {
                        return fail(
                            BenchError::Manifest("[defaults] parallel does not fit".to_owned()),
                            "load",
                        );
                    }
                },
                max_repairs: count(defaults, "max_repairs")?,
            },
            profiles,
            arms,
            borrowed,
            tasks,
            exclusions,
            protocol: settings(only(&tables, "protocol")?),
        };
        bench.validate()?;
        Ok(bench)
    }

    /// Refuses a manifest whose names collide, are unsafe, or point at nothing it declares.
    fn validate(&self) -> Result<(), BenchError> {
        distinct(
            "profile",
            &self
                .profiles
                .iter()
                .map(|item| item.name.as_str())
                .collect::<Vec<_>>(),
        )?;
        distinct(
            "arm",
            &self
                .arms
                .iter()
                .map(|item| item.name.as_str())
                .collect::<Vec<_>>(),
        )?;
        distinct(
            "task",
            &self
                .tasks
                .iter()
                .map(|item| item.name.as_str())
                .collect::<Vec<_>>(),
        )?;
        let pairs = self
            .exclusions
            .iter()
            .map(|item| (item.arm.as_str(), item.task.as_str()))
            .chain(
                self.borrowed
                    .iter()
                    .map(|item| (item.arm.as_str(), item.task.as_str())),
            );
        for (arm, task) in pairs {
            if self.task(task).is_none() || self.arm(arm).is_none() {
                return fail(
                    BenchError::Manifest(format!(
                        "{arm}/{task} names an arm or task the manifest does not declare"
                    )),
                    "validate",
                );
            }
        }
        if self.arm(&self.mutant_arm).is_none() {
            return fail(
                BenchError::Manifest(format!(
                    "mutant_arm {:?} is not a declared arm",
                    self.mutant_arm
                )),
                "validate",
            );
        }
        Ok(())
    }

    /// The task called `name`.
    pub(super) fn task(&self, name: &str) -> Option<&Task> {
        self.tasks.iter().find(|task| task.name == name)
    }

    /// The arm called `name`.
    pub(super) fn arm(&self, name: &str) -> Option<&Arm> {
        self.arms.iter().find(|arm| arm.name == name)
    }

    /// The profile called `name`.
    pub(super) fn profile(&self, name: &str) -> Option<&Profile> {
        self.profiles.iter().find(|profile| profile.name == name)
    }

    /// The surface whose reference solution `arm`'s dry run compiles for `task`.
    pub(super) fn reference_surface(&self, arm: &Arm, task: &str) -> String {
        match self
            .borrowed
            .iter()
            .find(|lend| lend.arm == arm.name && lend.task == task)
        {
            Some(lend) => lend.surface.clone(),
            None => arm.reference.clone(),
        }
    }

    /// Whether the benchmark declines to run `arm` on `task`.
    pub(super) fn excludes(&self, arm: &str, task: &str) -> bool {
        self.exclusions
            .iter()
            .any(|exclusion| exclusion.arm == arm && exclusion.task == task)
    }

    /// A path inside the benchmark directory.
    pub(super) fn path(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{Bench, Setting, fill};
    use crate::sim_kit::{BENCH_DIR, COMMITTED, committed};

    /// Whether parsing `edited` is refused with a message naming `needle`.
    fn refused(edited: &str, needle: &str) -> bool {
        match Bench::parse(Path::new(BENCH_DIR), edited) {
            Ok(_) => false,
            Err(error) => error.to_string().contains(needle),
        }
    }

    /// bench/ai-authoring/README.md and tasks.md: six arms, six tasks, five
    /// required profiles, and the three cells the old and futures sheets cannot
    /// express (#270).
    #[test]
    fn the_committed_manifest_declares_the_benchmark_the_readme_describes()
    -> Result<(), Box<dyn std::error::Error>> {
        let bench = committed()?;
        let arms: Vec<&str> = bench.arms.iter().map(|arm| arm.name.as_str()).collect();
        assert_eq!(
            arms,
            ["old", "new", "fan", "futures", "script", "lexicon"],
            "the six arms, in the order the README tables list them"
        );
        assert_eq!(bench.tasks.len(), 6, "six held-out tasks");
        assert_eq!(bench.profiles.len(), 5, "the five required user profiles");
        assert_eq!(
            bench.exclusions.len(),
            3,
            "three cells no sheet can express"
        );
        assert_eq!(
            bench.defaults.trials, 1,
            "one trial per profile: two trials of one cell put the same prompt twice, a repeat in meaning the Director's order of 2026-10-17 forbids"
        );
        assert!(
            bench.limits.token_budget < 10_000_000
                && bench.limits.token_budget >= bench.limits.call_tokens,
            "the budget is under the Director's 10M tokens and holds at least one reply"
        );
        assert!(
            bench.excludes("old", "capture") && bench.excludes("futures", "recovery"),
            "the old sheet has no process spawn and the futures sheet no durable store"
        );
        assert!(
            bench
                .protocol
                .iter()
                .any(|statement| statement.0 == "hidden_oracle"
                    && matches!(statement.1, Setting::Flag(true))),
            "the protocol states the oracle is hidden as a flag"
        );
        Ok(())
    }

    /// The lexicon arm authors `script!` from the generated lexicon, and the fan
    /// arm's recovery reference is a new-surface solution (trial.py, 2026-10-11).
    #[test]
    fn a_reference_comes_from_the_arm_then_from_what_it_borrows()
    -> Result<(), Box<dyn std::error::Error>> {
        let bench = committed()?;
        let lexicon = bench.arm("lexicon").ok_or("no lexicon arm")?;
        let fan = bench.arm("fan").ok_or("no fan arm")?;
        assert_eq!(
            bench.reference_surface(lexicon, "aggregate"),
            "script",
            "the lexicon arm compiles the script reference"
        );
        assert_eq!(
            bench.reference_surface(fan, "recovery"),
            "new",
            "fan borrows the new reference for recovery"
        );
        assert_eq!(
            bench.reference_surface(fan, "aggregate"),
            "fan",
            "fan keeps its own reference elsewhere"
        );
        Ok(())
    }

    /// A benchmark that names a file that is not there fails its first trial,
    /// hours into a run; every file the manifest and its layout name must exist.
    #[test]
    fn every_file_the_manifest_names_exists() -> Result<(), Box<dyn std::error::Error>> {
        let bench = committed()?;
        let mut missing = Vec::new();
        let mut want = |path: std::path::PathBuf| {
            if !path.is_file() {
                missing.push(path.display().to_string());
            }
        };
        want(bench.path(&bench.layout.lock));
        want(bench.path(&bench.layout.template).join("Cargo.toml"));
        for arm in &bench.arms {
            want(bench.path(&arm.sheet));
        }
        for task in &bench.tasks {
            want(bench.path(&fill(&bench.layout.prompt, &[("task", &task.name)])));
            want(bench.path(&fill(&bench.layout.oracle, &[("task", &task.name)])));
            want(bench.path(&fill(&bench.layout.mutant, &[("task", &task.name)])));
            want(bench.path(&fill(
                &bench.layout.reference,
                &[("surface", &task.mutant_base), ("task", &task.name)],
            )));
            for arm in &bench.arms {
                if !bench.excludes(&arm.name, &task.name) {
                    let surface = bench.reference_surface(arm, &task.name);
                    want(bench.path(&fill(
                        &bench.layout.reference,
                        &[("surface", &surface), ("task", &task.name)],
                    )));
                }
            }
        }
        for entry in &bench.template.paths {
            if !bench.path(&entry.target).is_dir() {
                missing.push(entry.target.clone());
            }
        }
        assert!(
            missing.is_empty(),
            "the manifest names files that are not there: {missing:?}"
        );
        Ok(())
    }

    #[test]
    fn a_name_declared_twice_is_refused() {
        assert!(
            refused(
                &COMMITTED.replacen("name = \"fan\"", "name = \"old\"", 1),
                "declared twice"
            ),
            "two arms called old"
        );
    }

    /// A name becomes a directory, a package and a file name, so one with a
    /// separator or a dot-dot would let a manifest reach outside its work root.
    #[test]
    fn a_name_that_could_leave_its_directory_is_refused() {
        assert!(
            refused(
                &COMMITTED.replacen("name = \"fan\"", "name = \"../escape\"", 1),
                "use only"
            ),
            "a path separator in an arm name"
        );
        assert!(
            refused(
                &COMMITTED.replacen("name = \"fan\"", "name = \"..\"", 1),
                "use only"
            ),
            "a dot-dot arm name"
        );
    }

    #[test]
    fn a_missing_key_is_refused_naming_it() {
        assert!(
            refused(&COMMITTED.replacen("framing =", "framed =", 1), "framing"),
            "the reply contract is required"
        );
    }

    #[test]
    fn an_exclusion_of_an_undeclared_cell_is_refused() {
        let edited = COMMITTED.replacen(
            "task = \"recovery\"\narm = \"old\"",
            "task = \"nonesuch\"\narm = \"old\"",
            1,
        );
        assert!(
            refused(&edited, "does not declare"),
            "an exclusion of a task nobody declared"
        );
    }

    #[test]
    fn a_limit_that_is_not_a_positive_whole_number_is_refused() {
        // The committed value is the one replaced, so a limit changed in the
        // manifest cannot leave this edit a no-op that the test then misreads.
        let edited = COMMITTED.replacen("oracle_s = 300", "oracle_s = 0", 1);
        assert_ne!(
            edited, COMMITTED,
            "the committed manifest declares `oracle_s = 300`"
        );
        assert!(
            refused(&edited, "oracle_s"),
            "an oracle bound of zero seconds would time out every trial"
        );
    }

    #[test]
    fn fill_replaces_every_named_placeholder_and_only_those() {
        assert_eq!(
            fill("tasks/{task}/{task}.rs {other}", &[("task", "aggregate")]),
            "tasks/aggregate/aggregate.rs {other}",
            "each occurrence of a named placeholder is replaced and an unnamed one is left"
        );
    }
}
