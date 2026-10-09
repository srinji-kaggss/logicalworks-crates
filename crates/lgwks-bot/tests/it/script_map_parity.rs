//! The tool's reading of a script is the map the compiler built from it (#384).
//!
//! `lgwks-ast script map` finds every `script!` in a file by lexing the file
//! and reads each through `lgwks_ast::script::parse`, the function the macro
//! itself calls. This file holds that reading to the `ARCHITECTURE` the macro
//! compiled for the same invocation: the rendered map and the JSON document
//! must be equal, line numbers included. A tool that read a script differently
//! from the compiler, or numbered its lines differently, fails here and not in
//! a reader's head.
//!
//! This binary's own scripts are compared in process. The rest are files this
//! binary cannot hold: the examples (each loads `tests/support/lock.rs`, which
//! this binary already loads, and a file loaded twice is refused) and the
//! AI-authoring bench's reference solutions (built against the bench's support
//! crate). Each becomes a module of one probe that `cargo check` type-checks,
//! with the tool's reading of it written in as `const` assertions on that
//! module's `ARCHITECTURE`. The compiler evaluates them, so a divergence is a
//! compile error naming the file, the flow and the step. The references are
//! found by reading the directory, not listed, so a new one is covered the day
//! it is added.

#![cfg(feature = "script")]

use std::error::Error;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use lgwks_ast::script::{Script, StepShape, read_source};
use lgwks_bot::script::Architecture;
use lgwks_std::json::Value;

use crate::compile::check_consumer;

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

/// Every `script!` this binary compiles: the source file it is written in,
/// read as text, and the map the macro compiled from it.
const COMPILED: [(&str, &str, Architecture); 2] = [
    (
        "tests/it/script_flow.rs",
        include_str!("script_flow.rs"),
        crate::script_flow::ARCHITECTURE,
    ),
    (
        "tests/it/sim_script.rs",
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

/// The message of the one assertion the probe makes that must fail.
const CONTROL: &str = "parity control: the probe's assertions were evaluated";

/// Byte equality of two strings, usable in a `const` (the probe's own copy).
const SAME: &str = "const fn same(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut index = 0;
    while index < a.len() {
        if a[index] != b[index] {
            return false;
        }
        index += 1;
    }
    true
}
";

/// Write one `const` assertion: `condition` must hold, or the compile fails
/// with `label`.
fn fact(out: &mut String, condition: &str, label: &str) -> std::fmt::Result {
    writeln!(out, "    assert!({condition}, {label:?});")
}

/// Write the assertions for `steps` at `path` (an expression naming the
/// compiled slice), labelled `at`, recursing into each step.
fn assert_steps(out: &mut String, path: &str, at: &str, steps: &[StepShape]) -> std::fmt::Result {
    let count = format!("{path}.len() == {}", steps.len());
    fact(out, &count, &format!("{at}: the number of steps"))?;
    for (index, step) in steps.iter().enumerate() {
        let here = format!("{path}[{index}]");
        let label = format!("{at}.{index}");
        let kind = format!(
            "matches!({here}.kind(), lgwks_bot::script::StepKind::{:?})",
            step.kind()
        );
        fact(out, &kind, &format!("{label}: kind"))?;
        let subject = format!("same({here}.subject(), {:?})", step.subject());
        fact(out, &subject, &format!("{label}: subject"))?;
        let detail = format!("same({here}.detail(), {:?})", step.detail());
        fact(out, &detail, &format!("{label}: detail"))?;
        let line = format!("{here}.line() == {}", step.line());
        fact(out, &line, &format!("{label}: line"))?;
        assert_steps(out, &format!("{here}.steps()"), &label, step.children())?;
    }
    Ok(())
}

/// The probe's source: each file as a module, the tool's reading of it as
/// `const` assertions on its `ARCHITECTURE`, and one assertion that fails.
fn probe_source(files: &[(PathBuf, Script)]) -> Result<String, std::fmt::Error> {
    let mut main = String::new();
    for (index, reference) in files.iter().enumerate() {
        let shown = reference.0.display().to_string();
        writeln!(main, "#[path = {shown:?}]\nmod reference_{index};")?;
        writeln!(
            main,
            "const _: () = {{\n    let map = reference_{index}::ARCHITECTURE;"
        )?;
        let flows = reference.1.flows();
        let count = format!("map.flows().len() == {}", flows.len());
        fact(&mut main, &count, &format!("{shown}: the number of flows"))?;
        for (number, flow) in flows.iter().enumerate() {
            let shape = flow.shape();
            let path = format!("map.flows()[{number}]");
            let at = format!("{shown} flow {}", shape.name());
            let name = format!("same({path}.name(), {:?})", shape.name());
            fact(&mut main, &name, &format!("{at}: name"))?;
            let signature = format!("same({path}.signature(), {:?})", shape.signature());
            fact(&mut main, &signature, &format!("{at}: signature"))?;
            let line = format!("{path}.line() == {}", shape.line());
            fact(&mut main, &line, &format!("{at}: line"))?;
            assert_steps(&mut main, &format!("{path}.steps()"), &at, shape.steps())?;
        }
        main.push_str("};\n");
    }
    writeln!(main, "const _: () = assert!(false, {CONTROL:?});")?;
    main.push_str(SAME);
    main.push_str("fn main() {}\n");
    Ok(main)
}

/// The examples that hold a `script!`, by file name under `examples/`.
const EXAMPLES: [&str; 2] = ["compare_orchestration.rs", "script_tenants.rs"];

#[test]
fn the_tool_reads_each_example_and_bench_reference_as_the_map_it_compiles_to() -> TestResult {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).canonicalize()?;
    let bench = manifest.join("../../bench/ai-authoring").canonicalize()?;
    let mut listing: Vec<PathBuf> = std::fs::read_dir(bench.join("reference"))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<_, _>>()?;
    listing.sort();
    let mut files: Vec<(PathBuf, Script)> = Vec::new();
    for example in EXAMPLES {
        let path = manifest.join("examples").join(example);
        let script = the_one_script(example, &std::fs::read_to_string(&path)?)?;
        files.push((path, script));
    }
    for path in listing {
        let source = std::fs::read_to_string(&path)?;
        if !read_source(&source)?.is_empty() {
            let script = the_one_script(&path.display().to_string(), &source)?;
            files.push((path, script));
        }
    }
    assert!(
        files.len() >= EXAMPLES.len() + 6,
        "the examples and the bench's script references were found: {}",
        files.len()
    );
    // `process` beside the default features is the set `t22_process_surface`
    // already type-checks, so this probe reuses that build of `lgwks_bot`;
    // `ai_task_support`'s narrower request unifies into it.
    let dependencies = format!(
        "lgwks_bot = {{ path = {:?}, features = [\"process\"] }}\n\
         lgwks_std = {{ path = {:?} }}\n\
         ai_task_support = {{ path = {:?} }}\n",
        manifest.display().to_string(),
        manifest
            .join("../lgwks-std")
            .canonicalize()?
            .display()
            .to_string(),
        bench.join("support").display().to_string()
    );
    let output = check_consumer("script-map-parity", &dependencies, &probe_source(&files)?)?;
    let printed = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    // Exactly one error, and it is the control: the compiler evaluated the
    // assertions (the control fired) and every one of the tool's held.
    assert!(
        !output.status.success() && printed.contains(CONTROL),
        "the probe's assertions were evaluated:\n{printed}"
    );
    assert!(
        printed.contains("due to 1 previous error"),
        "the tool's reading of every file is the ARCHITECTURE it compiles to:\n{printed}"
    );
    Ok(())
}
