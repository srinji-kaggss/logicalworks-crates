//! The compile cache the runners put in front of rustc, run as cargo runs it:
//! the real `sccache`, the real `cargo`, and the environment the `local-rust`
//! action gives every step (#738, runs 39960738059 to 39974411070).
//!
//! The requirement is that an instance reads a compile another instance made. The
//! action exports `CARGO_TARGET_DIR`, a different directory for each instance;
//! cargo gives rustc its whole environment; and sccache 0.18.0 hashes every
//! `CARGO_` variable it is given (mozilla/sccache issue 2494). So with plain
//! `sccache` as `RUSTC_WRAPPER` the same crate built into a second target
//! directory is a miss, and with the program the action builds from
//! `rustc-sccache/main.rs` it is a hit. Both are asserted: the control run keeps
//! this test from passing on a cache that never misses. Each scenario starts a
//! server of its own on a free port, with a cache directory of its own, so nothing
//! here reads or changes the machine's server, and the drop guard stops it.

use crate::replay::{Failure, refuse};

use std::fs;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The wrapper's source, which the action builds with the pinned `rustc` alone.
const WRAPPER_SOURCE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../.github/actions/local-rust/rustc-sccache/main.rs"
);

/// A directory of this run's own, made empty: the family and a random draw name
/// it, so two runs on one machine never share one.
fn scratch(family: &str) -> Result<PathBuf, Failure> {
    let distinguisher = u64::from_le_bytes(lgwks_std::random::bytes::<8>()?);
    let dir = std::env::temp_dir().join(format!("sccache-key-it-{family}-{distinguisher:016x}"));
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Removes a directory this run made.
fn clear(dir: &Path) {
    match fs::remove_dir_all(dir) {
        Ok(()) | Err(_) => {}
    }
}

/// An sccache server of this scenario's own: its own port, its own cache.
struct Server {
    /// The port the server listens on.
    port: u16,
    /// Where the server keeps its cache.
    dir: PathBuf,
}

impl Server {
    /// Starts a server on a free port with its cache under `dir`.
    fn start(dir: &Path) -> Result<Self, Failure> {
        let port = TcpListener::bind(("127.0.0.1", 0))?.local_addr()?.port();
        let server = Self {
            port,
            dir: dir.to_path_buf(),
        };
        let status = server
            .sccache()
            .arg("--start-server")
            .status()
            .map_err(|cause| {
                format!("sccache must be on PATH, it is the runners' compile cache: {cause}")
            })?;
        if !status.success() {
            return refuse(format!("sccache --start-server exited {status}"));
        }
        Ok(server)
    }

    /// The `sccache` command, aimed at this server.
    fn sccache(&self) -> Command {
        let mut command = Command::new("sccache");
        command
            .env("SCCACHE_SERVER_PORT", self.port.to_string())
            .env("SCCACHE_DIR", &self.dir)
            .env("SCCACHE_CACHE_SIZE", "64M");
        command
    }

    /// The Rust compiles this server has served and has not.
    fn rust_counts(&self) -> Result<(u64, u64), Failure> {
        let output = self.sccache().arg("--show-stats").output()?;
        let text = String::from_utf8(output.stdout)?;
        Ok((
            counted(&text, "Cache hits (Rust)")?,
            counted(&text, "Cache misses (Rust)")?,
        ))
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        match self.sccache().arg("--stop-server").output() {
            Ok(_) | Err(_) => {}
        }
    }
}

/// The count on the statistics line that starts with `label`; sccache leaves a
/// line out when its count is zero.
fn counted(stats: &str, label: &str) -> Result<u64, Failure> {
    for line in stats.lines() {
        if let Some(rest) = line.strip_prefix(label) {
            return Ok(rest.trim().parse()?);
        }
    }
    Ok(0)
}

/// A library of one source file and no dependencies, under `dir`.
fn project(dir: &Path) -> Result<PathBuf, Failure> {
    let project = dir.join("project");
    fs::create_dir_all(project.join("src"))?;
    fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )?;
    fs::write(project.join("src/lib.rs"), "pub fn f() -> u32 { 1 }\n")?;
    Ok(project)
}

/// The wrapper, built from its source as the action builds it: `rustc` alone.
fn wrapper(dir: &Path) -> Result<PathBuf, Failure> {
    let program = dir.join("rustc-sccache");
    let output = Command::new("rustc")
        .args(["--edition", "2021", WRAPPER_SOURCE, "-o"])
        .arg(&program)
        .output()?;
    if !output.status.success() {
        return refuse(format!(
            "rustc could not build the wrapper: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(program)
}

/// Builds the project's library into `target` with `wrapper` as `RUSTC_WRAPPER`,
/// in the environment the action gives a step: the instance's target directory in
/// `CARGO_TARGET_DIR`, and nothing else from this process but the paths cargo and
/// rustup need to find their toolchain.
fn build(server: &Server, project: &Path, target: &Path, wrapper: &Path) -> Result<(), Failure> {
    let mut command = Command::new("cargo");
    command.current_dir(project).env_clear();
    for name in ["PATH", "HOME", "RUSTUP_HOME", "CARGO_HOME", "TMPDIR"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    let output = command
        .args(["build", "--lib", "--offline"])
        .env("RUSTC_WRAPPER", wrapper)
        .env("CARGO_TARGET_DIR", target)
        .env("CARGO_INCREMENTAL", "0")
        .env("SCCACHE_SERVER_PORT", server.port.to_string())
        .output()?;
    if !output.status.success() {
        return refuse(format!(
            "cargo build exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(())
}

/// One project built into two target directories under `dir`, through the wrapper
/// or, when `use_wrapper` is false, through plain `sccache`; the Rust hits and
/// misses the server counted. The server stops when this returns.
fn scenario(dir: &Path, use_wrapper: bool) -> Result<(u64, u64), Failure> {
    let project = project(dir)?;
    let program = if use_wrapper {
        wrapper(dir)?
    } else {
        PathBuf::from("sccache")
    };
    let server = Server::start(&dir.join("cache"))?;
    build(&server, &project, &dir.join("target-1"), &program)?;
    build(&server, &project, &dir.join("target-2"), &program)?;
    server.rust_counts()
}

/// [`scenario`] in a directory of its own, removed afterwards.
fn second_target(family: &str, use_wrapper: bool) -> Result<(u64, u64), Failure> {
    let dir = scratch(family)?;
    let outcome = scenario(&dir, use_wrapper);
    clear(&dir);
    outcome
}

/// Two instances build one crate: the second reads the first's compile through
/// the wrapper, and misses through plain sccache, which is why the wrapper exists.
#[test]
fn the_wrapper_lets_a_second_target_directory_read_the_first_ones_compile() -> Result<(), Failure> {
    let (plain_hits, plain_misses) = second_target("plain", false)?;
    if plain_hits != 0 || plain_misses < 2 {
        return refuse(format!(
            "the control no longer shows the defect: plain sccache hit {plain_hits} and missed \
             {plain_misses} compiles across two target directories; if sccache now ignores \
             CARGO_TARGET_DIR the wrapper is not needed"
        ));
    }
    let (hits, misses) = second_target("wrapped", true)?;
    if hits < 1 || misses < 1 {
        return refuse(format!(
            "the wrapper did not share the compile across target directories: {hits} hits, \
             {misses} misses (one miss for the first build and a hit for the second)"
        ));
    }
    Ok(())
}
