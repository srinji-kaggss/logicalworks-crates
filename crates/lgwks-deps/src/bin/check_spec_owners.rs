//! The `check-spec-owners` command: reads its root argument, then runs the one
//! implementation in `lgwks_deps::spec_owners` (#695).

use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    // The one argument is the root; with none, the current directory.
    let mut root = PathBuf::from(".");
    if let Some(argument) = std::env::args_os().nth(1) {
        root = PathBuf::from(argument);
    }
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let stderr = io::stderr();
    let mut err = stderr.lock();
    match lgwks_deps::spec_owners::execute(&root, &mut out, &mut err) {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            if writeln!(err, "{error}").is_err() {
                return ExitCode::from(2);
            }
            ExitCode::from(1)
        }
    }
}
