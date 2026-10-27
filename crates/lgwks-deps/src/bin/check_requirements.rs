//! The `check-requirements` command: reads its arguments, then runs the one
//! implementation in `lgwks_deps::requirements` (#695).

use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut update = false;
    let mut root = PathBuf::from(".");
    for argument in std::env::args_os().skip(1) {
        if argument == "--update" {
            update = true;
        } else {
            root = PathBuf::from(argument);
        }
    }
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let stderr = io::stderr();
    let mut err = stderr.lock();
    match lgwks_deps::requirements::execute(&root, update, &mut out, &mut err) {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            if writeln!(err, "{error}").is_err() {
                return ExitCode::from(2);
            }
            ExitCode::from(1)
        }
    }
}
