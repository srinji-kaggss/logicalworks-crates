//! Simulated trial directories: every trial of the committed benchmark is set up in
//! a directory of its own, and what the model is shown, what the oracle names and
//! what a finished trial leaves behind are held to the files the benchmark commits.
//!
//! A trial that reads another trial's directory, a prompt that carries a sheet it
//! was not given, or an oracle that does not name the crate under test is a
//! measurement of something other than the arm, so each is checked on disk.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::Path;

use super::BenchError;
use super::author::{Texts, full_prompt};
use super::plan::{Job, Mode, package_for, trial_id};
use super::replay::{Failure, replay_sweep};
use super::scaffold::{
    TrialDir, oracle_for, oracle_text, place_reference, prepare, prompt_texts, reference_text,
    task_prompt, write_outputs, write_scratch,
};
use super::sim_kit::{BENCH_DIR, Scratch, committed, ensure, job, one_of};

/// Texts a file must keep byte for byte: empty, plain, CRLF, non-ASCII, a fence.
const TEXTS: [&str; 5] = [
    "",
    "plain\n",
    "crlf\r\nline\r\n",
    "snowman \u{2603} crab \u{1f980}\n",
    "backticks ``` in the middle\n",
];

/// The text of `relative` inside the trial's directory.
fn contents(dir: &TrialDir, relative: &str) -> Result<String, Failure> {
    Ok(fs::read_to_string(dir.path.join(relative))?)
}

/// The text of a file the benchmark commits, relative to its directory.
fn committed_file(relative: &str) -> Result<String, Failure> {
    Ok(fs::read_to_string(Path::new(BENCH_DIR).join(relative))?)
}

/// Source: the Cargo Book ("Specifying dependencies": a `path` dependency is
/// resolved from the manifest's directory) and the benchmark's own rule that a trial
/// is built `--locked --offline`. Requirement: a trial directory is the template
/// with its package name and its two paths filled, carrying the committed lock
/// with exactly its one name line rewritten; it is created new, a second set-up
/// of the same trial is refused with the first untouched, and another trial can
/// share the work root.
#[test]
fn a_trial_crate_is_the_template_filled_in_and_is_never_overwritten() -> Result<(), Failure> {
    let bench = committed()?;
    let template = committed_file(&format!("{}/Cargo.toml", bench.layout.template))?;
    let lock = committed_file(&bench.layout.lock)?;
    replay_sweep("prepare", 30, |rng, trace| {
        let scratch = Scratch::new("prepare")?;
        let first = job(rng, &bench, Mode::Models)?;
        let dir = prepare(&bench, scratch.path(), &first)?;
        ensure(
            dir.id == trial_id(&first)
                && dir.package == package_for(&bench.package_prefix, &dir.id)
                && dir.path == scratch.path().join(&dir.id)
                && dir.path.join("src").is_dir()
                && dir.scratch().is_dir(),
            || format!("the directory of {} is not laid out as a trial", dir.id),
        )?;
        let manifest = contents(&dir, "Cargo.toml")?;
        ensure(
            manifest.contains(&format!("name = \"{}\"", dir.package))
                && !manifest.contains(&bench.template.name_token)
                && manifest.lines().count() == template.lines().count(),
            || {
                format!(
                    "the manifest of {} was not filled from the template",
                    dir.id
                )
            },
        )?;
        for entry in &bench.template.paths {
            let resolved = fs::canonicalize(Path::new(BENCH_DIR).join(&entry.target))?;
            ensure(
                manifest.contains(&resolved.display().to_string())
                    && !manifest.contains(&entry.token),
                || {
                    format!(
                        "the path {} was not pointed at {}",
                        entry.token, entry.target
                    )
                },
            )?;
        }
        let rewritten = contents(&dir, "Cargo.lock")?;
        let changed = rewritten
            .lines()
            .zip(lock.lines())
            .filter(|pair| pair.0 != pair.1)
            .count();
        ensure(
            rewritten.lines().count() == lock.lines().count()
                && changed == 1
                && rewritten.contains(&format!("name = \"{}\"", dir.package)),
            || {
                format!(
                    "the lock of {} is not the committed lock with its name line rewritten",
                    dir.id
                )
            },
        )?;
        match prepare(&bench, scratch.path(), &first) {
            Err(BenchError::Io { ref cause, .. })
                if cause.kind() == io::ErrorKind::AlreadyExists => {}
            Ok(_) => return Err("a second set-up reused the trial directory".into()),
            Err(other) => {
                return Err(format!("the refusal was {other:?}, not an existing directory").into());
            }
        }
        ensure(contents(&dir, "Cargo.toml")? == manifest, || {
            "a refused set-up changed the trial".to_owned()
        })?;
        let next = Job {
            trial: first.trial.saturating_add(1),
            ..first
        };
        let other = prepare(&bench, scratch.path(), &next)?;
        ensure(other.path != dir.path, || {
            "two trials share a directory".to_owned()
        })?;
        trace.record(&dir.id);
        Ok(())
    })
}

/// Requirement: the model's scratch directory holds the prompt files and nothing
/// else (the task, the API sheet and, only when the profile has one, its role), and
/// a finished trial leaves its solution, the model's raw output and both logs
/// byte for byte.
#[test]
fn the_model_sees_only_its_prompt_files_and_a_finished_trial_keeps_its_four_outputs()
-> Result<(), Failure> {
    let bench = committed()?;
    replay_sweep("files", 30, |rng, trace| {
        let scratch = Scratch::new("files")?;
        let dir = prepare(&bench, scratch.path(), &job(rng, &bench, Mode::Models)?)?;
        let texts = Texts {
            preamble: (*one_of(rng, "preambles", &TEXTS)?).to_owned(),
            task: (*one_of(rng, "tasks", &TEXTS)?).to_owned(),
            sheet: (*one_of(rng, "sheets", &TEXTS)?).to_owned(),
        };
        write_scratch(&dir, &texts)?;
        let mut found = BTreeSet::new();
        for entry in fs::read_dir(dir.scratch())? {
            found.insert(entry?.file_name().to_string_lossy().into_owned());
        }
        let mut wanted: BTreeSet<String> = ["prompt.md", "api.md"]
            .iter()
            .map(|name| (*name).to_owned())
            .collect();
        if !texts.preamble.is_empty() {
            wanted.insert("role.md".to_owned());
        }
        ensure(found == wanted, || {
            format!("the model sees {found:?}, expected {wanted:?}")
        })?;
        let read = |name: &str| -> Result<String, Failure> {
            Ok(fs::read_to_string(dir.scratch().join(name))?)
        };
        ensure(
            read("prompt.md")? == texts.task && read("api.md")? == texts.sheet,
            || "the prompt files are not the task and the sheet".to_owned(),
        )?;
        ensure(
            texts.preamble.is_empty() || read("role.md")? == texts.preamble,
            || "the role file is not the preamble".to_owned(),
        )?;
        let code = *one_of(rng, "code", &TEXTS)?;
        let raw = *one_of(rng, "raw", &TEXTS)?;
        let build = *one_of(rng, "build", &TEXTS)?;
        let test = *one_of(rng, "test", &TEXTS)?;
        write_outputs(&dir, code, raw, build, test)?;
        ensure(
            contents(&dir, "lib.rs")? == code
                && contents(&dir, "raw.ndjson")? == raw
                && contents(&dir, "build.log")? == build
                && contents(&dir, "test.log")? == test,
            || "a finished trial did not keep its outputs byte for byte".to_owned(),
        )?;
        trace.record_number("files", found.len());
        Ok(())
    })
}

/// Requirement: a trial's prompt is its profile's preamble, its task's prompt and
/// its arm's sheet, read from the committed files, closed by the reply contract; an
/// arm the manifest does not declare is a usage error, and a trial with no profile
/// has no preamble.
#[test]
fn a_prompt_is_made_of_the_committed_files_of_its_arm_task_and_profile() -> Result<(), Failure> {
    let bench = committed()?;
    replay_sweep("prompts", 100, |rng, trace| {
        let drawn = job(rng, &bench, Mode::Models)?;
        let texts = prompt_texts(&bench, &drawn)?;
        let arm = bench.arm(&drawn.arm).ok_or("the drawn arm is undeclared")?;
        let profile = bench
            .profile(&drawn.profile)
            .ok_or("the drawn profile is undeclared")?;
        ensure(
            texts.sheet == fs::read_to_string(bench.path(&arm.sheet))?
                && texts.preamble == profile.text
                && texts.task == task_prompt(&bench, &drawn.task)?,
            || {
                format!(
                    "the prompt of {} is not made of the committed files",
                    trial_id(&drawn)
                )
            },
        )?;
        let prompt = full_prompt(&bench.framing, &texts);
        ensure(
            prompt.starts_with(&bench.framing)
                && prompt.ends_with(&format!("{}\n", texts.sheet))
                && prompt.contains(&texts.task),
            || {
                format!(
                    "the prompt of {} is not framed, task, sheet",
                    trial_id(&drawn)
                )
            },
        )?;
        let stray = Job {
            arm: "nonesuch".to_owned(),
            ..drawn.clone()
        };
        match prompt_texts(&bench, &stray) {
            Err(BenchError::Usage(said)) => ensure(said.contains("is not a declared arm"), || {
                format!("an undeclared arm was refused as `{said}`")
            })?,
            Ok(_) => return Err("an undeclared arm was given a prompt".into()),
            Err(other) => {
                return Err(format!("the refusal was {other:?}, not a usage error").into());
            }
        }
        let bare = prompt_texts(
            &bench,
            &Job {
                profile: String::new(),
                ..drawn.clone()
            },
        )?;
        ensure(bare.preamble.is_empty(), || {
            "a trial with no profile got a preamble".to_owned()
        })?;
        trace.record(&prompt);
        Ok(())
    })
}

/// Source: #270, comment of 2026-10-08: "the lexicon-only cell (the author's whole
/// context is the generated `docs/script-lexicon.md`)". Requirement: the lexicon arm's
/// sheet is that file as committed, and the one API section of its prompt holds
/// it and nothing else, whatever the task and the profile.
#[test]
fn the_lexicon_arm_is_shown_the_generated_script_lexicon_and_nothing_else() -> Result<(), Failure> {
    let bench = committed()?;
    let arm = bench
        .arm("lexicon")
        .ok_or("the benchmark declares no lexicon arm")?;
    ensure(arm.sheet.ends_with("docs/script-lexicon.md"), || {
        format!(
            "the lexicon arm reads {}, not the generated lexicon",
            arm.sheet
        )
    })?;
    let lexicon = committed_file(&arm.sheet)?;
    ensure(!lexicon.trim().is_empty(), || {
        "the generated lexicon is empty".to_owned()
    })?;
    replay_sweep("lexicon", 100, |rng, trace| {
        let drawn = Job {
            arm: arm.name.clone(),
            ..job(rng, &bench, Mode::Models)?
        };
        let texts = prompt_texts(&bench, &drawn)?;
        ensure(texts.sheet == lexicon, || {
            "the lexicon arm's sheet is not the lexicon".to_owned()
        })?;
        let prompt = full_prompt(&bench.framing, &texts);
        ensure(
            prompt.matches("=== API SHEET ===").count() == 1
                && prompt.ends_with(&format!("=== API SHEET ===\n\n{lexicon}\n")),
            || "the lexicon arm's prompt carries more than the lexicon as its API".to_owned(),
        )?;
        trace.record_number("bytes", prompt.len());
        Ok(())
    })
}

/// Requirement: every oracle names the crate under test by the token the
/// manifest gives and reads back with that name replaced by the trial's package
/// (each occurrence once), has clauses with distinct names, and holds the drop
/// clause the manifest names for its task; without it a trial's cleanup would be
/// measured against nothing.
#[test]
fn every_oracle_names_the_crate_under_test_and_holds_its_drop_clause() -> Result<(), Failure> {
    let bench = committed()?;
    replay_sweep("oracles", 60, |rng, trace| {
        let task = one_of(rng, "tasks", &bench.tasks)?;
        let drawn = Job {
            task: task.name.clone(),
            ..job(rng, &bench, Mode::Models)?
        };
        let package = package_for(&bench.package_prefix, &trial_id(&drawn));
        let (text, clauses) = oracle_for(&bench, &task.name, &package)?;
        let original = oracle_text(&bench, &task.name)?;
        let named = original.matches(bench.oracle_token.as_str()).count();
        ensure(
            named > 0 && text.matches(package.as_str()).count() == named,
            || {
                format!(
                    "the oracle of {} does not name the crate under test once per token",
                    task.name
                )
            },
        )?;
        let distinct: BTreeSet<&String> = clauses.iter().collect();
        ensure(
            distinct.len() == clauses.len() && clauses.contains(&task.drop_clause),
            || {
                format!(
                    "the oracle of {} lacks its drop clause {} or repeats a clause",
                    task.name, task.drop_clause
                )
            },
        )?;
        ensure(
            clauses
                .iter()
                .all(|clause| text.contains(&format!("fn {clause}"))),
            || format!("a clause of {} is not a function of its oracle", task.name),
        )?;
        trace.record_number("clauses", clauses.len());
        Ok(())
    })
}

/// Requirement: a mutant is its reference plus one mutation. The reference is
/// placed beside it byte for byte as `src/reference.rs`; a task the manifest does
/// not declare is a usage error and places nothing.
#[test]
fn a_mutants_reference_is_placed_byte_for_byte_and_an_undeclared_task_places_nothing()
-> Result<(), Failure> {
    let bench = committed()?;
    replay_sweep("references", 30, |rng, trace| {
        let scratch = Scratch::new("references")?;
        let drawn = job(rng, &bench, Mode::Mutants)?;
        let dir = prepare(&bench, scratch.path(), &drawn)?;
        let task = bench
            .task(&drawn.task)
            .ok_or("the drawn task is undeclared")?;
        place_reference(&bench, &dir, &drawn)?;
        ensure(
            contents(&dir, "src/reference.rs")?
                == reference_text(&bench, &task.mutant_base, &drawn.task)?,
            || {
                format!(
                    "the reference of {} was not placed byte for byte",
                    drawn.task
                )
            },
        )?;
        let stray = Job {
            task: "nonesuch".to_owned(),
            ..drawn.clone()
        };
        match place_reference(&bench, &dir, &stray) {
            Err(BenchError::Usage(said)) => {
                ensure(said.contains("is not a declared task"), || {
                    format!("an undeclared task was refused as `{said}`")
                })?
            }
            Ok(()) => return Err("an undeclared task had a reference placed".into()),
            Err(other) => {
                return Err(format!("the refusal was {other:?}, not a usage error").into());
            }
        }
        trace.record(&drawn.task);
        Ok(())
    })
}
