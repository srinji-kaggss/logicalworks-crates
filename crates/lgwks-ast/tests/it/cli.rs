//! What every test that runs the shipped `lgwks-ast` binary shares: running it
//! in a scratch directory, reading its two streams as text, and planting a file.

#![cfg(feature = "tool")]

use std::error::Error;
use std::path::Path;
use std::process::{Command, Output};

/// Run the binary with `args` in `dir`.
pub(crate) fn tool(dir: &Path, args: &[&str]) -> Result<Output, Box<dyn Error>> {
    Ok(Command::new(env!("CARGO_BIN_EXE_lgwks-ast"))
        .args(args)
        .current_dir(dir)
        .output()?)
}

/// Stdout and stderr as text.
pub(crate) fn text(output: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// Write `contents` to `relative` under `root`, creating its directories.
pub(crate) fn plant(root: &Path, relative: &str, contents: &str) -> Result<(), Box<dyn Error>> {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, contents)?;
    Ok(())
}
