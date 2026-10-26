//! The `check-doc-citations` command: reads its arguments, then runs the one
//! implementation in `lgwks_deps::doc_citations` (#695).

use std::io::{self, Write};
use std::process::ExitCode;

fn main() -> ExitCode {
    let options = lgwks_deps::doc_citations::options();
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let stderr = io::stderr();
    let mut err = stderr.lock();
    match lgwks_deps::doc_citations::execute(&options, &mut out, &mut err) {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            if writeln!(err, "{error}").is_err() {
                return ExitCode::from(2);
            }
            ExitCode::from(1)
        }
    }
}
