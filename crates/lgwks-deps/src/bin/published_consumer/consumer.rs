//! The consumers: one package per crate and feature set, built from crates.io at
//! an exact version, with no path into this tree.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::path::Path;
use std::time::Duration;

use crate::Failure;
use crate::children::{Children, Launch};
use crate::console::Console;

/// How long one consumer may take to resolve, compile and run. A cold build of
/// `lgwks_bot` with every feature is the longest; a consumer past this is hung.
const BOUND: Duration = Duration::from_secs(1800);

/// How many lines of a failed consumer's log are shown.
const LOG_LINES: usize = 40;

/// What `lgwks_std`'s consumers run when its README has no runnable block.
const STD_MINIMAL: &str =
    "fn main() { assert_eq!(lgwks_std::hex::encode(&[1u8, 2, 3, 4]), \"01020304\"); }\n";

/// The modules `lgwks_deps` 4.0.0 keeps at its root. `scan` is not one of them:
/// it moved to `lgwks_ast` (#408), and importing it failed the 4.0.0 receipt.
pub(crate) const DEPS_IMPORTS: &str =
    "#[allow(unused_imports)]\nuse lgwks_deps::{contract, lock, metadata};\nfn main() {}\n";

/// What the `tokio-full` storefront consumer runs.
const DEPS_TOKIO: &str = "fn main() {
    let Ok(runtime) = lgwks_deps::tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        std::process::exit(1)
    };
    runtime.block_on(async { lgwks_deps::tokio::task::yield_now().await });
}
";

/// A program that only links the crate, for a crate whose pages hold no
/// runnable block at its tag.
const LINKS: &str = "fn main() {}\n";

/// What `drishti`'s consumer runs: its README holds no Rust block, so the
/// program reads the published palette, the library's own design tokens.
const DRISHTI_PALETTE: &str = "fn main() {
    use drishti::design::{Palette, Theme};
    assert_eq!(Palette::dark().theme, Theme::Dark);
    assert_eq!(Palette::light().theme, Theme::Light);
}
";

/// One consumer: a crate, a feature set, and where its program comes from.
pub(crate) struct Row {
    /// The crate's name on crates.io.
    pub(crate) name: &'static str,
    /// The crate's directory in this repository.
    pub(crate) dir: &'static str,
    /// The feature set's name, in the receipt and in the package name.
    pub(crate) label: &'static str,
    /// The rest of the dependency entry: `default-features`, `features`.
    pub(crate) spec: &'static str,
    /// The page of the crate that carries its runnable quickstart.
    pub(crate) page: &'static str,
    /// What runs when that page has no runnable block at the tag.
    pub(crate) minimal: &'static str,
}

/// Every consumer, in receipt order. `lgwks_bot`'s runnable flow is on its
/// script page: its `README.md` is the overview and holds no Rust block.
/// `lgwks_macros` is linked on its own, so a version that resolves only
/// through `lgwks_bot` is still caught.
pub(crate) const ROWS: [Row; 10] = [
    Row {
        name: "lgwks_std",
        dir: "crates/lgwks-std",
        label: "default",
        spec: "default-features = true",
        page: "README.md",
        minimal: STD_MINIMAL,
    },
    Row {
        name: "lgwks_std",
        dir: "crates/lgwks-std",
        label: "core",
        spec: "default-features = false, features = [\"core\"]",
        page: "README.md",
        minimal: STD_MINIMAL,
    },
    Row {
        name: "lgwks_std",
        dir: "crates/lgwks-std",
        label: "full",
        spec: "features = [\"full\"]",
        page: "README.md",
        minimal: STD_MINIMAL,
    },
    Row {
        name: "lgwks_ast",
        dir: "crates/lgwks-ast",
        label: "default",
        spec: "default-features = true",
        page: "README.md",
        minimal: LINKS,
    },
    Row {
        name: "lgwks_deps",
        dir: "crates/lgwks-deps",
        label: "default",
        spec: "default-features = true",
        page: "README.md",
        minimal: DEPS_IMPORTS,
    },
    Row {
        name: "lgwks_deps",
        dir: "crates/lgwks-deps",
        label: "tokio-full",
        spec: "default-features = false, features = [\"tokio-full\"]",
        page: "README.md",
        minimal: DEPS_TOKIO,
    },
    Row {
        name: "lgwks_bot",
        dir: "crates/lgwks-bot",
        label: "default",
        spec: "default-features = true",
        page: "README-script.md",
        minimal: LINKS,
    },
    Row {
        name: "lgwks_bot",
        dir: "crates/lgwks-bot",
        label: "full",
        spec: "features = [\"full\"]",
        page: "README-script.md",
        minimal: LINKS,
    },
    Row {
        name: "lgwks_macros",
        dir: "crates/lgwks-macros",
        label: "default",
        spec: "default-features = true",
        page: "README.md",
        minimal: LINKS,
    },
    Row {
        name: "drishti",
        dir: "crates/drishti",
        label: "default",
        spec: "default-features = true",
        page: "README.md",
        minimal: DRISHTI_PALETTE,
    },
];

/// The `Cargo.toml` of the consumer of `row` at exactly `version`.
pub(crate) fn manifest(row: &Row, version: &str) -> String {
    let package = row.name.replace('_', "-");
    format!(
        "[package]\nname = \"consumer-{package}-{label}\"\nversion = \"0.0.0\"\nedition = \"2024\"\npublish = false\n\n[dependencies]\n{name} = {{ version = \"={version}\", {spec} }}\n\n[workspace]\n",
        label = row.label,
        name = row.name,
        spec = row.spec,
    )
}

/// Builds and runs the consumer of `row` at `version` with `program`; whether it
/// passed. A failed consumer's log is shown. The package has no lock file by
/// design, so `--locked` has nothing to assert: a stranger resolves from the
/// registry, and the exact `=version` is the pin.
pub(crate) fn consume(
    children: &Children,
    console: &Console,
    env: &BTreeMap<String, String>,
    work: &Path,
    row: &Row,
    version: &str,
    program: &str,
) -> Result<bool, Failure> {
    let dir = work.join(format!("consumer-{}-{}", row.name, row.label));
    let source = dir.join("src");
    if let Err(error) = fs::create_dir_all(&source) {
        return Failure::io(&source, error);
    }
    for (path, text) in [
        (dir.join("Cargo.toml"), manifest(row, version)),
        (source.join("main.rs"), program.to_owned()),
    ] {
        if let Err(error) = fs::write(&path, text) {
            return Failure::io(&path, error);
        }
    }
    let log_path = dir.join("run.log");
    let log = match File::create(&log_path) {
        Ok(file) => file,
        Err(error) => return Failure::io(&log_path, error),
    };
    let argv = ["cargo".to_owned(), "run".to_owned(), "--quiet".to_owned()];
    let launch = Launch {
        argv: &argv,
        root: &dir,
        env,
        stdin: None,
        bound: BOUND,
    };
    if children.run(&launch, Some(&log)) == 0 {
        return Ok(true);
    }
    match fs::read_to_string(&log_path) {
        Ok(text) => {
            for line in text.lines().take(LOG_LINES) {
                console.warn(line);
            }
        }
        Err(error) => console.warn(&format!("{}: {error}", log_path.display())),
    }
    Ok(false)
}
