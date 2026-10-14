//! The Linux leg of the gate: the workspace suite, the `lgwks_bot` full-feature
//! suite and the AppCUI storefront, run on Linux.
//!
//! CI runs on the self-hosted macOS runners (Director, 2026-10-04). The
//! supervisor and `lgwks_std::process` have Linux-only paths (`/proc` child
//! lists, the Linux reap, Landlock), so a macOS-only gate would compile them and
//! run none. On macOS this runs the suites inside a container; on a Linux host
//! it runs them directly, which is the Linux the container existed to provide
//! (the container cost an image, a VM sharing the machine's cores, and 8 to 10
//! minutes of contended wall on the critical path).
//!
//! The container engine on macOS is OrbStack (Director, 2026-10-07): Docker
//! Desktop is not installed on the CI machine, and the leg refuses any other
//! engine rather than run on whichever one `docker` happens to reach. The image
//! is the repository's pinned toolchain (`rust-toolchain.toml`) on Debian
//! trixie, with `lld` and `sqlite3` added; nextest is the version the host
//! runs, so the two legs differ only in the operating system. Build output, the
//! registry and the nextest binary live in named volumes, so a run reuses what
//! the last run built. Each runner owns its target volume, and each suite set
//! has its own, so CI can run the sets side by side without one waiting on
//! another's build lock.
//!
//! `bot-full` runs under nextest's `ci` profile, which writes the JUnit report
//! the acceptance receipt is recorded from (#271): the workspace is mounted at
//! `/src`, so the report lands in this checkout's `target/nextest/ci/junit.xml`.
//!
//! The machine's cargo configuration is the build (Director, 2026-10-06: CI uses
//! every optimisation the global cargo config declares). The container reads the
//! same file, and the host target's rustflags (lld, target-cpu=native) are
//! applied to the container's target, so its own target table takes precedence
//! over `[build] rustflags` exactly as the host's does.
//!
//! Usage, from the repository root: `linux-container-tests [workspace|bot-full|appcui]...`
//! (default: all three). `PARTITION=count:N/M` shards the workspace suite.
//! Exit status is the first failing command's, or 2 when the leg could not run.

mod plan;
#[cfg(test)]
mod tests;
#[path = "../shared/toml_subset.rs"]
mod toml_subset;

use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, ExitStatus, Stdio};

use plan::{
    Container, SUITES, container_script, container_triple, dockerfile, host_triple, names,
    nextest_platform, nextest_version, owner, rustflags_variable, suite_commands, target_rustflags,
    toolchain_channel,
};

/// A condition that stops the leg, named rather than guessed.
#[derive(Debug)]
struct Failure(String);

impl fmt::Display for Failure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

impl std::error::Error for Failure {}

/// Logs `message` at the site that produced it, then hands it back as the `Err`.
fn fail<T>(message: String, site: &'static str) -> Result<T, Failure> {
    lgwks_std::trace::debug!(message = %message, site, "returning an error to the caller");
    Err(Failure(message))
}

/// The exit code of a finished command; 1 when a signal ended it.
fn exit_code(status: ExitStatus) -> u8 {
    match status.code().map(u8::try_from) {
        Some(Ok(code)) => code,
        Some(Err(_)) | None => 1,
    }
}

/// What `program` prints on standard output, trimmed, when it succeeds.
fn printed(program: &str, arguments: &[&str]) -> Result<String, Failure> {
    let output = match Command::new(program)
        .args(arguments)
        .stdin(Stdio::null())
        .output()
    {
        Ok(found) => found,
        Err(error) => return fail(format!("{program}: {error}"), "printed"),
    };
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned());
    }
    fail(
        format!(
            "`{program} {}` exited with {}",
            arguments.join(" "),
            output.status
        ),
        "printed",
    )
}

/// The user's home directory.
fn home() -> Result<PathBuf, Failure> {
    match std::env::var_os("HOME") {
        Some(found) if !found.is_empty() => Ok(PathBuf::from(found)),
        Some(_) | None => fail("HOME is not set".to_owned(), "home"),
    }
}

/// The Docker CLI. On macOS the engine is OrbStack, named by its socket rather
/// than by the current Docker context: a context is per-user client state that
/// another tool can switch, and a run whose engine depended on it found none at
/// all (run 37619560219 attempt 2). The CLI is OrbStack's own, from its app
/// bundle: the runners' PATH reached a `docker` only through Homebrew's Docker
/// formula, which is removed with the rest of Docker (run 37623747480: `docker:
/// command not found`).
fn docker() -> Result<Command, Failure> {
    if !cfg!(target_os = "macos") {
        return Ok(Command::new("docker"));
    }
    let home = home()?;
    let bin_dirs = [
        PathBuf::from("/Applications/OrbStack.app/Contents/MacOS/xbin"),
        home.join(".orbstack/bin"),
    ];
    let cli = bin_dirs
        .iter()
        .map(|dir| dir.join("docker"))
        .find(|candidate| candidate.is_file());
    let mut command = match cli {
        Some(path) => Command::new(path),
        None => Command::new("docker"),
    };
    let inherited = match std::env::var_os("PATH") {
        Some(found) => std::env::split_paths(&found).collect(),
        None => Vec::new(),
    };
    let search: Vec<PathBuf> = bin_dirs.iter().cloned().chain(inherited).collect();
    match std::env::join_paths(search) {
        Ok(joined) => {
            command.env("PATH", joined);
        }
        Err(error) => {
            lgwks_std::trace::debug!(%error, "the search path could not be joined");
        }
    }
    command.env(
        "DOCKER_HOST",
        format!("unix://{}/.orbstack/run/docker.sock", home.display()),
    );
    Ok(command)
}

/// Refuses an engine that is not OrbStack.
fn require_orbstack() -> Result<(), Failure> {
    let output = match docker()?
        .args(["info", "--format", "{{.OperatingSystem}}"])
        .stdin(Stdio::null())
        .output()
    {
        Ok(found) => found,
        Err(error) => return fail(format!("docker: {error}"), "require_orbstack"),
    };
    let engine = if output.status.success() {
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    } else {
        String::from_utf8_lossy(&output.stderr).trim().to_owned()
    };
    if output.status.success() && engine == "OrbStack" {
        return Ok(());
    }
    fail(
        format!(
            "no OrbStack engine: {engine}; start OrbStack (`orb start`), the Linux leg runs on OrbStack only"
        ),
        "require_orbstack",
    )
}

/// Builds the derived image. `docker build` reuses its cached layers, so an
/// unchanged Dockerfile costs a second and the apt mirror is reached once per
/// machine and toolchain: installing the linker inside every run fetched it
/// from the Debian mirror each time, and one slow mirror held a job for 6.5
/// minutes and then failed it with `cannot find 'ld'` (run 38316346444).
fn build_tools_image(image: &str, tools_image: &str) -> Result<(), Failure> {
    let mut child = match docker()?
        .args(["build", "-q", "-t", tools_image, "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
    {
        Ok(started) => started,
        Err(error) => return fail(format!("docker build: {error}"), "build_tools_image"),
    };
    if let Some(mut stdin) = child.stdin.take()
        && let Err(error) = stdin.write_all(dockerfile(image).as_bytes())
    {
        return fail(format!("docker build: {error}"), "build_tools_image");
    }
    match child.wait() {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => fail(
            format!("building {tools_image} from {image} exited with {status}"),
            "build_tools_image",
        ),
        Err(error) => fail(format!("docker build: {error}"), "build_tools_image"),
    }
}

/// Runs each command on this host, stopping at the first that fails.
fn run_on_host(commands: &[Vec<String>]) -> Result<u8, Failure> {
    for command in commands {
        let Some((program, arguments)) = command.split_first() else {
            continue;
        };
        match Command::new(program).args(arguments).status() {
            Ok(status) if status.success() => {}
            Ok(status) => return Ok(exit_code(status)),
            Err(error) => return fail(format!("{program}: {error}"), "run_on_host"),
        }
    }
    Ok(0)
}

/// Where cargo keeps its configuration: `CARGO_HOME`, else `~/.cargo`.
fn cargo_home() -> Result<PathBuf, Failure> {
    match std::env::var_os("CARGO_HOME") {
        Some(found) if !found.is_empty() => Ok(PathBuf::from(found)),
        Some(_) | None => Ok(home()?.join(".cargo")),
    }
}

/// The text of a file, naming it when it cannot be read.
fn read(path: &Path) -> Result<String, Failure> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(error) => fail(format!("{}: {error}", path.display()), "read"),
    }
}

/// The value of an environment variable, when set and not empty.
fn setting(name: &str) -> Option<String> {
    match std::env::var(name) {
        Ok(found) if !found.is_empty() => Some(found),
        Ok(_) | Err(_) => None,
    }
}

/// Runs the suites in the container.
fn run_in_container(suites: &[String], commands: &[Vec<String>]) -> Result<u8, Failure> {
    let root = match std::env::current_dir().and_then(std::fs::canonicalize) {
        Ok(found) => found,
        Err(error) => {
            return fail(
                format!("the working directory: {error}"),
                "run_in_container",
            );
        }
    };
    let manifest = read(&root.join("rust-toolchain.toml"))?;
    let Some(toolchain) = toolchain_channel(&manifest) else {
        return fail(
            "rust-toolchain.toml names no channel".to_owned(),
            "run_in_container",
        );
    };
    let nextest_output = printed("cargo", &["nextest", "--version"])?;
    let Some(nextest) = nextest_version(&nextest_output) else {
        return fail(
            format!("`cargo nextest --version` printed {nextest_output:?}"),
            "run_in_container",
        );
    };
    let arch = std::env::consts::ARCH;
    let Some(platform) = nextest_platform(arch) else {
        return fail(format!("no nextest build for {arch}"), "run_in_container");
    };
    let version = printed("rustc", &["-vV"])?;
    let Some(host) = host_triple(&version) else {
        return fail("`rustc -vV` names no host".to_owned(), "run_in_container");
    };
    let config_path = cargo_home()?.join("config.toml");
    let config = config_path.is_file().then_some(config_path);
    let flags = match config.as_deref() {
        Some(path) => target_rustflags(&read(path)?, host)?.join(" "),
        None => String::new(),
    };
    if cfg!(target_os = "macos") {
        require_orbstack()?;
    }
    let runner = setting("RUNNER_NAME");
    let docker_names = names(&toolchain, &owner(runner.as_deref()), suites);
    build_tools_image(&docker_names.image, &docker_names.tools_image)?;
    let script = container_script(nextest, platform, commands);
    let flags_variable = rustflags_variable(&container_triple(arch));
    let test_debug = match setting("CARGO_PROFILE_TEST_DEBUG") {
        Some(found) => found,
        None => "line-tables-only".to_owned(),
    };
    let root_text = root.display().to_string();
    let config_text = config.as_deref().map(|path| path.display().to_string());
    let container = Container {
        names: &docker_names,
        root: &root_text,
        cargo_config: config_text.as_deref(),
        flags_variable: &flags_variable,
        flags: &flags,
        test_debug: &test_debug,
        script: &script,
    };
    match docker()?.args(container.arguments()).status() {
        Ok(status) => Ok(exit_code(status)),
        Err(error) => fail(format!("docker run: {error}"), "run_in_container"),
    }
}

/// Runs with `arguments` and returns the exit status.
fn execute(arguments: &[String]) -> Result<u8, Failure> {
    let suites: Vec<String> = if arguments.is_empty() {
        SUITES.iter().map(|suite| (*suite).to_owned()).collect()
    } else {
        arguments.to_vec()
    };
    let partition = setting("PARTITION");
    let mut commands = Vec::new();
    for suite in &suites {
        match suite_commands(suite, partition.as_deref()) {
            Some(found) => commands.extend(found),
            None => {
                return fail(
                    format!("unknown suite {suite} (workspace, bot-full, appcui)"),
                    "execute",
                );
            }
        }
    }
    if cfg!(target_os = "linux") {
        return run_on_host(&commands);
    }
    run_in_container(&suites, &commands)
}

/// Writes a line, logging a failure to write.
fn say(stream: &mut dyn Write, text: &str) {
    if let Err(error) = writeln!(stream, "{text}") {
        lgwks_std::trace::debug!(%error, "a line could not be written");
    }
}

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match execute(&arguments) {
        Ok(code) => ExitCode::from(code),
        Err(failure) => {
            let stderr = std::io::stderr();
            say(
                &mut stderr.lock(),
                &format!("linux-container-tests: {failure}"),
            );
            ExitCode::from(2)
        }
    }
}
