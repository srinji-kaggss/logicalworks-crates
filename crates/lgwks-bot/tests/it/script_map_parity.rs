//! The tool's reading of a script is the map the compiler built from it (#384).
//!
//! `lgwks-ast script map` finds every `script!` in a file by lexing the file
//! and reads each through `lgwks_ast::script::parse`, the function the macro
//! itself calls. This file holds that reading to the `ARCHITECTURE` the macro
//! compiled for the same invocation in this binary: the rendered map and the
//! JSON document must be equal, line numbers included, for every `script!`
//! compiled here. A tool that read a script differently from the compiler, or
//! numbered its lines differently, fails here and not in a reader's head.
//!
//! The examples' scripts compile into their own binaries, which this binary
//! cannot link, so each example is run with `map`, which prints its
//! `ARCHITECTURE` and runs nothing, and the tool's map is held to that text.
//!
//! The AI-authoring bench's reference solutions hold the rest. They build in
//! the bench's own Cargo root, so every reference that holds a `script!` is
//! compiled as a module of one probe binary against the bench's lockfile, and
//! each `ARCHITECTURE` it prints is held to the tool's reading of that file.
//! The references are found by reading the directory, not listed, so a new one
//! is covered the day it is added.

#![cfg(feature = "script")]

use std::error::Error;

use lgwks_ast::script::{Script, read_source};
use lgwks_bot::script::Architecture;
use lgwks_std::json::Value;

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::compile::{run_example, run_probe};

type TestResult = Result<(), Box<dyn Error>>;

/// The one script in `source`, as the tool reads it.
fn the_one_script(file: &str, source: &str) -> Result<Script, Box<dyn Error>> {
    let invocations = read_source(source)?;
    let [ref invocation] = *invocations.as_slice() else {
        let failure = format!(
            "{file}: the tool found {} script! invocations, and the file compiles one",
            invocations.len()
        );
        lgwks_std::trace::debug!(error = %failure, "the_one_script: returning an error to the caller");
        return Err(failure.into());
    };
    let script = invocation.read().map_err(|refusal| {
        format!("{file}: the tool refused what the compiler built: {refusal}")
    })?;
    Ok(script.clone())
}

/// Every example that holds a `script!`: its name, its source, and the
/// features it requires (its `[[example]]` entry's `required-features`).
const EXAMPLES: [(&str, &str, &str); 2] = [
    (
        "compare_orchestration",
        include_str!("../../examples/compare_orchestration.rs"),
        "script rt time sync macros",
    ),
    (
        "script_tenants",
        include_str!("../../examples/script_tenants.rs"),
        "script",
    ),
];

/// Every `script!` this binary compiles: the source file it is written in,
/// read as text, and the map the macro compiled from it.
const COMPILED: [(&str, &str, Architecture); 2] = [
    (
        "script_flow.rs",
        include_str!("script_flow.rs"),
        crate::script_flow::ARCHITECTURE,
    ),
    (
        "sim_script.rs",
        include_str!("sim_script.rs"),
        crate::sim_script::ARCHITECTURE,
    ),
];

#[test]
fn the_tool_reads_every_compiled_script_as_its_architecture() -> TestResult {
    for (file, source, compiled) in COMPILED {
        let script = the_one_script(file, source)?;
        assert_eq!(
            script.to_string(),
            compiled.to_string(),
            "{file}: the tool's map is the compiled ARCHITECTURE, line for line"
        );
        let compiled_json: Value = lgwks_std::json::from_str(&compiled.to_json()?)?;
        assert_eq!(
            script.to_json(),
            compiled_json,
            "{file}: the tool's JSON is the compiled ARCHITECTURE's JSON"
        );
        assert!(
            !compiled.flows().is_empty(),
            "{file}: a map with no flows would compare equal to an empty reading"
        );
    }
    Ok(())
}

#[test]
fn the_tool_reads_each_example_as_the_map_it_prints() -> TestResult {
    for (example, source, features) in EXAMPLES {
        let script = the_one_script(example, source)?;
        let output = run_example(example, features, &["map"])?;
        let printed = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "{example} map ran:\n{printed}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!printed.is_empty(), "{example} printed a map");
        assert_eq!(
            script.to_string(),
            printed,
            "{example}: the tool's map is the ARCHITECTURE the example compiled"
        );
    }
    Ok(())
}

/// The separator the probe prints before each reference's map.
const MARK: &str = "=== reference ";

#[test]
fn the_tool_reads_each_bench_reference_as_the_map_it_compiles_to() -> TestResult {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).canonicalize()?;
    let bench = manifest.join("../../bench/ai-authoring").canonicalize()?;
    let mut references: Vec<(PathBuf, Script)> = Vec::new();
    let mut listing: Vec<PathBuf> = std::fs::read_dir(bench.join("reference"))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<_, _>>()?;
    listing.sort();
    for path in listing {
        let source = std::fs::read_to_string(&path)?;
        if !read_source(&source)?.is_empty() {
            let script = the_one_script(&path.display().to_string(), &source)?;
            references.push((path, script));
        }
    }
    assert!(
        references.len() >= 6,
        "the bench's script references were found: {}",
        references.len()
    );
    let mut main = String::new();
    for (index, reference) in references.iter().enumerate() {
        let shown = reference.0.display().to_string();
        writeln!(main, "#[path = {shown:?}]\nmod reference_{index};")?;
    }
    main.push_str("fn main() {\n");
    for index in 0..references.len() {
        writeln!(
            main,
            "    print!(\"{MARK}{index}\\n{{}}\", reference_{index}::ARCHITECTURE);"
        )?;
    }
    main.push_str("}\n");
    let dependencies = format!(
        "lgwks_bot = {{ path = {:?}, default-features = false, features = [\"script\", \"process\"] }}\n\
         ai_task_support = {{ path = {:?} }}\n",
        manifest.display().to_string(),
        bench.join("support").display().to_string()
    );
    let output = run_probe(
        "bench-reference-maps",
        &dependencies,
        &bench.join("Cargo.lock"),
        &main,
    )?;
    let printed = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "the references compiled and printed their maps:\n{printed}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let maps: Vec<&str> = printed.split(MARK).skip(1).collect();
    assert_eq!(
        maps.len(),
        references.len(),
        "one map per reference:\n{printed}"
    );
    for (index, (map, reference)) in maps.iter().zip(&references).enumerate() {
        assert_eq!(
            format!("{index}\n{}", reference.1),
            *map,
            "{}: the tool's map is the ARCHITECTURE the reference compiled",
            reference.0.display()
        );
    }
    Ok(())
}
