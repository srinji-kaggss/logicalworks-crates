//! `drishti`: the command over drishti's reference core.
//!
//! Every subcommand prints one JSON document on stdout, so a person reads it
//! and an agent parses it with no mode switch:
//!
//! ```text
//! drishti tokens                          brand primitives, roles, type, motion and layout
//! drishti ink [--seed N] [--frames N] [--reduce-motion]
//!                                         every effect stepped on its own seeded engine
//! drishti present [--seed N] [--frames N] a residency session through loss and recovery
//! drishti limits                          the engine's and the runtime's declared bounds
//! drishti docs [PAGE]                     a page of the design publication, or the index
//! drishti lab [--seed N] [--frames N] [--dark] [--reduce-motion]
//!                                         the native window (feature `ui`)
//! ```
//!
//! `ink` and `present` run as `script!` flows: every effect is one body of a
//! bounded fan-out under a deadline, and a session is one deadline-bounded step,
//! each under the `drishti` tenant's scope.

mod ink;
mod present;
mod tokens;

use std::fmt;
use std::io::Write as _;

use drishti::ink_lab::Effect;
use drishti::{
    architecture, assurance, codebase, contracts, decisions, gpu_runtime_documentation,
    inklab_documentation, research, status, vision,
};
use lgwks_bot::script::{FlowError, Scope, Tenant};
use lgwks_std::json::{self, Value, json};

lgwks_bot::script! {
    /// Step every effect on its own seeded engine, as many at once as the
    /// machine sustains; each effect gets ten seconds.
    pub flow ink_effects(seed: u64, frames: u32, reduce_motion: bool) -> Vec<Value>:
        let runs = each effect in Effect::all():
            within 10s:
                ink::run(effect, seed, frames, reduce_motion)
        give back runs

    /// One residency session, under a thirty-second deadline.
    pub flow residency(seed: u64, frames: u32) -> Value:
        let session = within 30s:
            present::session(seed, frames)
        give back session
}

/// Why the command refused.
#[derive(Debug)]
enum CliError {
    /// No subcommand, or one this command does not have.
    Usage,
    /// A flag this subcommand does not take, or one missing its value.
    Flag(String),
    /// A flag value that is not a number in range.
    Value {
        /// The flag.
        flag: &'static str,
        /// What was given.
        given: String,
    },
    /// A page the publication does not have.
    Page(String),
    /// A flow stopped: past its deadline, or cancelled.
    Flow(FlowError),
    /// The document could not be encoded.
    Json(json::Error),
    /// Stdout refused the document.
    Io(std::io::Error),
}

impl fmt::Display for CliError {
    /// One line naming what to do instead.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Usage => formatter.write_str(USAGE),
            Self::Flag(ref flag) => {
                write!(formatter, "unknown or incomplete flag {flag:?}\n{USAGE}")
            }
            Self::Value { flag, ref given } => {
                write!(
                    formatter,
                    "{flag} takes a whole number in range, not {given:?}"
                )
            }
            Self::Page(ref page) => {
                write!(
                    formatter,
                    "no page {page:?}; `drishti docs` lists the pages"
                )
            }
            Self::Flow(ref error) => write!(formatter, "the flow stopped: {error}"),
            Self::Json(ref error) => write!(formatter, "the document did not encode: {error}"),
            Self::Io(ref error) => write!(formatter, "stdout refused the document: {error}"),
        }
    }
}

impl std::error::Error for CliError {
    /// The cause, where there is one.
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match *self {
            Self::Flow(ref error) => Some(error),
            Self::Json(ref error) => Some(error),
            Self::Io(ref error) => Some(error),
            Self::Usage | Self::Flag(_) | Self::Value { .. } | Self::Page(_) => None,
        }
    }
}

impl From<FlowError> for CliError {
    /// A stopped flow.
    fn from(error: FlowError) -> Self {
        Self::Flow(error)
    }
}

/// The help text, printed for a missing or unknown subcommand.
const USAGE: &str = "usage: drishti <tokens | ink | present | limits | docs [PAGE] | lab> \
                     [--seed N] [--frames N] [--reduce-motion] [--dark]";

/// Frames a session runs when `--frames` is not given: two seconds at 60 Hz.
const DEFAULT_FRAMES: u32 = 120;

/// The most frames one invocation runs: ten minutes at 60 Hz.
const MAX_FRAMES: u32 = 36_000;

/// Flags shared by the subcommands that step the engine.
#[derive(Clone, Copy, Debug)]
struct Flags {
    /// Master seed: one seed replays the same geometry.
    seed: u64,
    /// Frames to step.
    frames: u32,
    /// Resolve every effect to its finished frame.
    reduce_motion: bool,
    /// The graphite theme (the native window only).
    dark: bool,
}

/// Read `--seed`, `--frames`, `--reduce-motion` and `--dark`.
fn flags(words: &[String]) -> Result<Flags, CliError> {
    let mut parsed = Flags {
        seed: 1,
        frames: DEFAULT_FRAMES,
        reduce_motion: false,
        dark: false,
    };
    let mut rest = words.iter();
    while let Some(word) = rest.next() {
        match word.as_str() {
            "--reduce-motion" => parsed.reduce_motion = true,
            "--dark" => parsed.dark = true,
            "--seed" => {
                let given = rest.next().ok_or_else(|| CliError::Flag(word.clone()))?;
                parsed.seed = match given.parse::<u64>() {
                    Ok(seed) => seed,
                    Err(error) => {
                        lgwks_std::trace::debug!(%given, %error, "drishti: --seed refused");
                        return Err(value("--seed", given));
                    }
                };
            }
            "--frames" => {
                let given = rest.next().ok_or_else(|| CliError::Flag(word.clone()))?;
                parsed.frames = match given.parse::<u32>() {
                    Ok(frames) if frames <= MAX_FRAMES => frames,
                    Ok(_) | Err(_) => {
                        lgwks_std::trace::debug!(
                            %given,
                            max = MAX_FRAMES,
                            "drishti: --frames refused"
                        );
                        return Err(value("--frames", given));
                    }
                };
            }
            _ => {
                lgwks_std::trace::debug!(%word, "drishti: unknown or incomplete flag");
                return Err(CliError::Flag(word.clone()));
            }
        }
    }
    Ok(parsed)
}

/// A flag value refused: what the flag takes is in the message, so the parse
/// error's own text adds nothing a caller can act on.
fn value(flag: &'static str, given: &str) -> CliError {
    CliError::Value {
        flag,
        given: given.to_owned(),
    }
}

/// Every page of the design publication: its name and its Markdown.
const PAGES: [(&str, &str); 10] = [
    ("vision", vision::SOURCE),
    ("architecture", architecture::SOURCE),
    ("codebase", codebase::SOURCE),
    ("decisions", decisions::SOURCE),
    ("research", research::SOURCE),
    ("assurance", assurance::SOURCE),
    ("status", status::SOURCE),
    ("inklab", inklab_documentation::SOURCE),
    ("gpu-runtime", gpu_runtime_documentation::SOURCE),
    ("contracts", contracts::SOURCE),
];

/// `drishti docs [PAGE]`: one page with its Markdown, or the index with each
/// page's title.
fn docs(page: Option<&String>) -> Result<Value, CliError> {
    let Some(page) = page else {
        let index: Vec<Value> = PAGES
            .iter()
            .map(|&(name, text)| {
                json!({ "page": name, "title": text.lines().next(), "bytes": text.len() })
            })
            .collect();
        return Ok(json!({ "pages": index }));
    };
    match PAGES.iter().find(|&&(name, _)| name == page.as_str()) {
        Some(&(name, text)) => Ok(json!({ "page": name, "markdown": text })),
        None => Err(CliError::Page(page.clone())),
    }
}

/// The scope every flow of this command runs under.
fn scope() -> Result<Scope, CliError> {
    Ok(Scope::root(Tenant::new("drishti")?))
}

/// Run one subcommand to its document.
fn document(words: &[String]) -> Result<Value, CliError> {
    let Some((command, rest)) = words.split_first() else {
        lgwks_std::trace::debug!("drishti: no subcommand given");
        return Err(CliError::Usage);
    };
    match command.as_str() {
        "tokens" => Ok(tokens::document()),
        "limits" => Ok(present::limits()),
        "docs" => docs(rest.first()),
        "ink" => {
            let parsed = flags(rest)?;
            let runs = lgwks_bot::block_on(ink_effects(
                &scope()?,
                parsed.seed,
                parsed.frames,
                parsed.reduce_motion,
            ))?;
            Ok(json!({
                "seed": parsed.seed,
                "frames": parsed.frames,
                "reduce_motion": parsed.reduce_motion,
                "effects": runs,
            }))
        }
        "present" => {
            let parsed = flags(rest)?;
            Ok(lgwks_bot::block_on(residency(
                &scope()?,
                parsed.seed,
                parsed.frames,
            ))?)
        }
        "lab" => lab(flags(rest)?),
        _ => Err(CliError::Usage),
    }
}

/// `drishti lab`: the native ink lab window. Its session report is printed
/// when the window closes or the frame limit is reached.
#[cfg(feature = "ui")]
fn lab(parsed: Flags) -> Result<Value, CliError> {
    let mut launch = drishti::native::Launch::new();
    launch.seed = parsed.seed;
    launch.reduce_motion = parsed.reduce_motion;
    launch.dark = parsed.dark;
    launch.frame_limit = Some(u64::from(parsed.frames));
    drishti::native::run(launch, |outcome| {
        let document = match outcome {
            Ok(report) => present::session_report(&report),
            Err(error) => present::native_refusal(&error),
        };
        // The event loop owns the process from here, so a refused write has
        // no caller left to return to: it is reported as a broken contract.
        if let Err(error) = emit(&document) {
            lgwks_std::trace::error!(%error, "drishti lab: the session report was not written");
        }
    });
    Ok(json!({ "lab": "closed" }))
}

/// `drishti lab` without the `ui` feature: say how to get the window.
#[cfg(not(feature = "ui"))]
fn lab(parsed: Flags) -> Result<Value, CliError> {
    Ok(json!({
        "refused": "the native window needs the `ui` feature: cargo run -p drishti --features ui -- lab",
        "seed": parsed.seed,
        "frames": parsed.frames,
        "dark": parsed.dark,
        "reduce_motion": parsed.reduce_motion,
    }))
}

/// Write one document to stdout as pretty JSON.
fn emit(document: &Value) -> Result<(), CliError> {
    let text = json::to_string_pretty(document).map_err(CliError::Json)?;
    let mut out = std::io::stdout().lock();
    writeln!(out, "{text}").map_err(CliError::Io)
}

/// Run the command. A refusal is one JSON line on stderr naming what to do
/// instead, and the exit status says it failed.
fn main() -> std::process::ExitCode {
    let words: Vec<String> = std::env::args().skip(1).collect();
    let Err(error) = document(&words).and_then(|document| emit(&document)) else {
        return std::process::ExitCode::SUCCESS;
    };
    let refusal = json!({ "refused": error.to_string() });
    let mut err = std::io::stderr().lock();
    if let Err(unwritten) = writeln!(err, "{refusal}") {
        lgwks_std::trace::error!(%unwritten, %error, "drishti: the refusal was not written");
    }
    std::process::ExitCode::FAILURE
}
