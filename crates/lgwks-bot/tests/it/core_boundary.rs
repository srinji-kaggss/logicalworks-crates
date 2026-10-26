//! ADR 0006 decision 4: the core does not import the driver, and only `ecs/`
//! names the ECS engine.
//!
//! The ECS world is the core; tokio and axum are the one driver below it. ADR
//! 0005 held the same boundary as `review only`, and two pull requests (#734,
//! #735) built a tokio stack beside the world without a test refusing them.
//! This is that test: it reads every source file of the crate and refuses
//!
//! - a file outside `src/ecs.rs` and `src/ecs/` that names `bevy`, and
//! - a core module (`ecs`, `verb`, `cap`, `gate`, `effect`, `journal`,
//!   `error`, `clock`, `spec`) that names tokio, axum, or the crate's `rt`,
//!   `task`, `script` or `domain` modules.
//!
//! Comments are not code: a line is read up to its first `//`. Tests are not
//! the core either: a test drives the core from above, the way the driver
//! does, so a test file (`tests.rs`, a `tests/` directory, `sim_*`) and the
//! `#[cfg(test)] mod` that closes a file are not read.

use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

/// The modules ADR 0006 names as the core.
const CORE: [&str; 9] = [
    "ecs", "verb", "cap", "gate", "effect", "journal", "error", "clock", "spec",
];

/// What a core module may not name: the driver and the stacks that sit on it.
const DRIVER: [&str; 6] = [
    "tokio",
    "axum",
    "crate::rt",
    "crate::task",
    "crate::script",
    "crate::domain",
];

/// Whether `relative` (a `/`-separated path below `src/`) is the module `name`
/// or a file inside it.
fn in_module(relative: &str, name: &str) -> bool {
    relative == format!("{name}.rs") || relative.starts_with(&format!("{name}/"))
}

/// The code of one line: everything before its first `//`.
fn code_of(line: &str) -> &str {
    match line.split_once("//") {
        Some((code, _)) => code,
        None => line,
    }
}

/// Whether `relative` is a test file rather than shipped code.
fn is_test_file(relative: &str) -> bool {
    relative.split('/').any(|part| {
        part == "tests"
            || part == "tests.rs"
            || part.starts_with("sim_")
            || part.starts_with("test_")
    })
}

/// Whether line `index` of `lines` opens a `#[cfg(test)]` module.
fn opens_test_module(lines: &[&str], index: usize) -> bool {
    let Some(line) = lines.get(index) else {
        return false;
    };
    line.trim() == "#[cfg(test)]"
        && lines
            .iter()
            .skip(index.saturating_add(1))
            .find(|next| !next.trim().is_empty())
            .is_some_and(|next| next.trim_start().starts_with("mod "))
}

/// Every boundary edge in one source file, as `path:line: reason`.
fn edges(relative: &str, text: &str) -> Vec<String> {
    let core = CORE.iter().any(|name| in_module(relative, name));
    let ecs = in_module(relative, "ecs");
    let mut found = Vec::new();
    if is_test_file(relative) {
        return found;
    }
    let lines: Vec<&str> = text.lines().collect();
    for (index, line) in lines.iter().enumerate() {
        if opens_test_module(&lines, index) {
            break;
        }
        let number = index.saturating_add(1);
        let code = code_of(line);
        if !ecs && code.contains("bevy") {
            found.push(format!("{relative}:{number}: names bevy outside ecs/"));
        }
        if core {
            for driver in DRIVER.iter().filter(|driver| code.contains(**driver)) {
                found.push(format!("{relative}:{number}: core module names {driver}"));
            }
        }
    }
    found
}

/// Every `.rs` file under `dir`, recursively, in a stable order.
fn sources(dir: &Path, into: &mut Vec<PathBuf>) -> Result<(), Box<dyn Error>> {
    let mut entries = fs::read_dir(dir)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(fs::DirEntry::path);
    for entry in entries {
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            sources(&path, into)?;
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            into.push(path);
        }
    }
    Ok(())
}

#[test]
/// The shipped tree holds the boundary: no edge in any source file.
fn the_core_names_no_driver_and_only_ecs_names_bevy() -> Result<(), Box<dyn Error>> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    sources(&src, &mut files)?;
    assert!(files.len() > 100, "the walk found the crate's sources");
    let mut found = Vec::new();
    for file in &files {
        let relative = file
            .strip_prefix(&src)?
            .components()
            .map(|part| part.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        found.extend(edges(&relative, &fs::read_to_string(file)?));
    }
    assert!(
        found.is_empty(),
        "ADR 0006 boundary edges:\n{}",
        found.join("\n")
    );
    Ok(())
}

#[test]
/// ADR 0006's falsifier: a planted `use crate::rt` in `gate.rs` is refused.
fn a_planted_driver_import_in_the_core_is_refused() {
    assert_eq!(
        edges("gate.rs", "use crate::rt::runtime::Runtime;"),
        vec![String::from("gate.rs:1: core module names crate::rt")],
    );
    assert_eq!(
        edges("journal/file/write.rs", "use lgwks_deps::tokio::fs;").len(),
        1
    );
    assert_eq!(
        edges("ecs/bot.rs", "use lgwks_deps::axum::Router;").len(),
        1
    );
}

#[test]
/// bevy is refused everywhere but `ecs/`, and a comment is not an edge.
fn bevy_outside_ecs_is_refused_and_comments_are_not_code() {
    assert_eq!(
        edges("task/host.rs", "use lgwks_deps::bevy_ecs::World;").len(),
        1
    );
    assert!(edges("ecs/fold.rs", "use lgwks_deps::bevy_ecs::World;").is_empty());
    assert!(edges("ecs.rs", "use lgwks_deps::bevy_ecs::World;").is_empty());
    assert!(edges("gate.rs", "/// built on tokio // and crate::rt").is_empty());
    assert!(edges("rt/runtime.rs", "use lgwks_deps::tokio;").is_empty());
    assert!(
        edges("verbose.rs", "use crate::rt;").is_empty(),
        "verb is a module, not a prefix"
    );
}

#[test]
/// A test drives the core from above and is not read; the code before a test
/// module still is.
fn test_code_is_a_caller_and_production_code_above_it_is_still_read() {
    assert!(edges("journal/file/tests.rs", "use crate::rt::runtime::block_on;").is_empty());
    assert!(edges("clock/tests/wait.rs", "use crate::rt::time;").is_empty());
    assert!(edges("sim_gate.rs", "use crate::task::Host;").is_empty());
    let owner = "fn flush() {}\n\n#[cfg(test)]\nmod tests {\n    use crate::task::RunStore;\n}\n";
    assert!(
        edges("journal/owner.rs", owner).is_empty(),
        "a closing test module is a caller"
    );
    let planted = "use crate::script::Scope;\n#[cfg(test)]\nmod tests {}\n";
    assert_eq!(
        edges("journal/owner.rs", planted),
        vec![String::from(
            "journal/owner.rs:1: core module names crate::script"
        )],
        "an edge above the test module is still refused"
    );
    let gated_item = "#[cfg(test)]\nfn helper() {}\nuse crate::rt::time;\n";
    assert_eq!(
        edges("clock.rs", gated_item).len(),
        1,
        "only a test module closes the read"
    );
}
