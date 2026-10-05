//! T22's negative proof: a public process description must not be executable.

#![cfg(all(
    unix,
    feature = "rt",
    feature = "time",
    feature = "sync",
    feature = "process"
))]

use std::process::Output;

use lgwks_bot::Runtime;
use lgwks_bot::rt::process::ProcessSpec;
use lgwks_bot::rt::supervise::{Supervisor, TaskOutcome};
use lgwks_bot::rt::time::{Instant, sleep};

type TestResult = Result<(), Box<dyn std::error::Error>>;

// The compile-probe harness, shared with `t02_compile_surface`: one definition
// of where a nested build puts its artifacts, so the two targets cannot drift.
use crate::compile;

use compile::{assert_refused_for, compile_probe};

/// The probe compiled: the same harness, the same lockfile, the same features.
///
/// The positive control every negative probe needs. A refusal is only evidence
/// if the harness *could* have accepted a correct consumer, and the way that
/// fails is invisibly: an offline resolution error or a missing toolchain also
/// produces `!status.success()`, indistinguishable from the intended refusal
/// unless a correct program is shown to compile through the identical path.
fn assert_accepted(output: &Output, description: &str) {
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.status.success(),
        "the positive control ({description}) must compile, so a refusal below is \
         the compiler's judgement about the symbol and not this harness failing \
         to build the probe:\n{text}"
    );
}

/// Run one positive control: a correct consumer of the surface under test.
fn compiles(name: &str, dependency: &str, description: &str, main: &str) -> TestResult {
    let output = compile_probe(name, dependency, main)?;
    assert_accepted(&output, description);
    Ok(())
}

/// Run one negative probe and require the named refusal from the compiler.
fn refused(name: &str, dependency: &str, code: &str, symbol: &str, main: &str) -> TestResult {
    let output = compile_probe(name, dependency, main)?;
    assert_refused_for(&output, code, symbol);
    Ok(())
}

/// The feature set every probe in this file builds against.
///
/// One constant rather than four spellings of the same list: the positive and
/// negative halves of one row must differ only in the program they compile, and
/// a pair that had drifted apart in features would be two different experiments
/// under one name.
const PROCESS_FEATURES: &str = ", features = [\"process\"]";

#[test]
fn public_process_description_rejects_direct_execution() -> TestResult {
    refused(
        "t22-process-probe",
        PROCESS_FEATURES,
        "E0603",
        "Command",
        "use lgwks_bot::rt::process::Command;\n\nfn main() {\n    let mut command = Command::new(\"true\");\n    let _ = command.spawn();\n}\n",
    )
}

/// A description cannot be asked for the status of a run it did not perform.
///
/// `status` is the shape a caller reaches for the moment it wants a handle it
/// never got. The `ProcessSpec` carries no child, so there is no status to read
/// and the method is absent from the type rather than returning a placeholder —
/// a method that returned `None` would be a method a caller could learn to
/// ignore.
#[test]
fn a_process_description_cannot_report_a_status() -> TestResult {
    refused(
        "t22-status-probe",
        PROCESS_FEATURES,
        "E0599",
        "status",
        "use lgwks_bot::rt::process::ProcessSpec;\n\nfn main() {\n    let mut spec = ProcessSpec::new(\"sh\");\n    let _ = spec.status();\n}\n",
    )
}

/// A description cannot hand back a child's output either.
///
/// The other half of the same hole: `output` would be the one method that turns
/// a data description into a source of bytes, and it is absent for the same
/// reason `status` is — nothing about the spec has ever run.
#[test]
fn a_process_description_cannot_produce_output() -> TestResult {
    refused(
        "t22-output-probe",
        PROCESS_FEATURES,
        "E0599",
        "output",
        "use lgwks_bot::rt::process::ProcessSpec;\n\nfn main() {\n    let mut spec = ProcessSpec::new(\"sh\");\n    let _ = spec.output();\n}\n",
    )
}

/// The private engine command is not reachable through a mutable handle.
///
/// Every other escape on this row is a method; this one is the field shape. A
/// `pub(crate)` field is what keeps the engine's own `Command` — and therefore
/// `Command::spawn` — out of reach, and a `&mut` accessor to it would be the same
/// hole wearing a different hat. Both a mutating accessor and an owned one are
/// refused, because handing out either would let a caller start the child the
/// supervisor is supposed to own.
#[test]
fn a_process_description_exposes_no_mutable_engine_handle() -> TestResult {
    refused(
        "t22-engine-mut-probe",
        PROCESS_FEATURES,
        "E0599",
        "engine_mut",
        "use lgwks_bot::rt::process::ProcessSpec;\n\nfn main() {\n    let mut spec = ProcessSpec::new(\"sh\");\n    let _ = spec.engine_mut();\n}\n",
    )
}

/// `ProcessSpec` is not a smart pointer to anything.
///
/// The escape that a method probe cannot see: a `Deref` impl would hand every
/// method of some inner engine to a caller holding only a description, and no
/// list of absent methods would ever catch it. `ProcessSpec` is a plain record,
/// so both `&spec` and `&mut spec` fail to dereference — and the `&mut` arm is
/// the one that matters, because a `Deref` to the engine would be a mutable
/// handle by another spelling.
#[test]
fn a_process_description_cannot_deref_to_an_engine() -> TestResult {
    refused(
        "t22-deref-probe",
        PROCESS_FEATURES,
        "E0614",
        "cannot be dereferenced",
        "use lgwks_bot::rt::process::ProcessSpec;\n\nfn main() {\n    let spec = ProcessSpec::new(\"sh\");\n    let _shared: &ProcessSpec = &*spec;\n    let mut spec = ProcessSpec::new(\"sh\");\n    let _mutable: &mut ProcessSpec = &mut *spec;\n}\n",
    )
}

/// The same harness accepts a correct consumer of the description's real surface.
///
/// Every refusal above is paired with this. The four probes differ from this
/// program only in which symbol they name, so a green refusal is the compiler
/// answering about that symbol: if this control failed, all five would "pass" for
/// one reason that has nothing to do with the row.
#[test]
fn the_readable_description_surface_compiles_for_an_external_consumer() -> TestResult {
    compiles(
        "t22-description-control",
        PROCESS_FEATURES,
        "the inspection surface of ProcessSpec",
        "use lgwks_bot::rt::process::{ProcessSpec, StdioPolicy};\n\nfn main() {\n    let mut spec = ProcessSpec::new(\"sh\");\n    spec.arg(\"-c\").arg(\"exit 0\");\n    let _ = spec.program();\n    let _ = spec.args();\n    let _ = spec.stdout_policy();\n    let _ = spec.deadline_duration();\n    let _ = StdioPolicy::default();\n}\n",
    )
}

/// And the sanctioned runner — the one path that does execute — compiles too.
///
/// The negative probes prove the description is inert; this proves the crate has
/// not simply removed the capability. `Supervisor` takes the description by
/// borrow and keeps the engine handle itself, which is the whole design: a
/// consumer can describe work, and only the supervisor performs it.
#[test]
fn the_sanctioned_runner_accepts_the_same_description() -> TestResult {
    compiles(
        "t22-runner-control",
        PROCESS_FEATURES,
        "the supervised runner that owns the engine handle",
        "use lgwks_bot::rt::process::ProcessSpec;\nuse lgwks_bot::rt::supervise::Supervisor;\n\nfn main() {\n    let mut spec = ProcessSpec::new(\"sh\");\n    spec.arg(\"-c\").arg(\"exit 0\");\n    let _supervisor = Supervisor::new(1);\n    let _ = format!(\"{:?}\", spec.program());\n}\n",
    )
}

#[test]
fn the_guaranteed_task_set_does_not_expose_detach_all() -> TestResult {
    // A public consumer must not be able to detach work from the facade that
    // promises to own it. This is a real downstream compile probe, not a
    // source-text check: the method must be absent from the exported type.
    let output = compile_probe(
        "lgwks-bot-taskset-probe",
        "",
        "use lgwks_bot::rt::task::JoinSet;\nfn main() { let mut tasks = JoinSet::<()>::new(); tasks.detach_all(); }\n",
    )?;
    assert_refused_for(&output, "E0599", "detach_all");
    Ok(())
}

#[test]
fn supervisor_is_the_sanctioned_process_runner() -> TestResult {
    let runtime = Runtime::new()?;
    let outcome = runtime.block_on(async {
        let mut supervisor = Supervisor::default();
        let mut command = ProcessSpec::new("sh");
        command.arg("-c").arg("exit 0");
        supervisor.spawn_process(&command).await?;
        let deadline = Instant::now()
            .checked_add(std::time::Duration::from_secs(5))
            .ok_or_else(|| std::io::Error::other("clock deadline overflowed"))?;
        loop {
            supervisor.reap();
            if let Some(outcome) = supervisor.next_report() {
                break Ok::<_, std::io::Error>(outcome);
            }
            if Instant::now() >= deadline {
                break Err(std::io::Error::other(
                    "supervisor did not report the process",
                ));
            }
            sleep(std::time::Duration::from_millis(5)).await;
        }
    })?;
    assert!(matches!(outcome, TaskOutcome::Completed { .. }));
    Ok(())
}
