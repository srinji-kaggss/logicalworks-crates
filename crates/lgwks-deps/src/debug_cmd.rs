//! The `debug` command: the cargo-doctor-style report on a repository's
//! structured-debugging install.

use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use lgwks_deps::repository_root;

use crate::{refuse, subject_path};

// ── debug ──────────────────────────────────────────────────────────────────

/// Parsed arguments for the debugger doctor.
pub(crate) struct DebugArgs {
    /// Repository to inspect. `None` means the process working directory.
    target: Option<PathBuf>,
    /// True when the doctor report is written as JSON.
    json: bool,
}

/// A check that must hold for the debugger to be default-on.
pub(crate) struct DebugSurface {
    /// `default = [...]` includes `trace`.
    default_includes_trace: bool,
    /// `trace = [...]` includes `dep:tracing`.
    trace_includes_tracing: bool,
    /// `trace = [...]` includes `dep:tracing-subscriber`.
    trace_includes_subscriber: bool,
    /// The `tracing` dependency is declared.
    tracing_declared: bool,
    /// The `tracing-subscriber` dependency is declared.
    subscriber_declared: bool,
}

impl DebugSurface {
    /// True when no caller has to select an optional feature to get debugging.
    const fn passes(&self) -> bool {
        self.default_includes_trace
            && self.trace_includes_tracing
            && self.trace_includes_subscriber
            && self.tracing_declared
            && self.subscriber_declared
    }
}

/// Complete doctor report.
pub(crate) struct DebugReport {
    /// Repository root inspected by the doctor.
    root: PathBuf,
    /// Service name installed into the debugger.
    service_name: String,
    /// Filter directive installed into the debugger.
    filter: String,
    /// Output format installed into the debugger.
    format: lgwks_std::trace::DebugFormat,
    /// Manifest-level checks.
    surface: DebugSurface,
    /// True when the report should render as JSON.
    json: bool,
}

/// Parses `lgwks-deps debug` arguments.
pub(crate) fn parse_debug_args(args: &[String]) -> Result<DebugArgs, String> {
    let mut target: Option<PathBuf> = None;
    let mut json = false;
    for argument in args {
        match argument.as_str() {
            "--json" => json = true,
            flag if flag.starts_with("--") => {
                let refusal = Err(format!("unknown option for `debug`: {flag}"));
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "parse_debug_args: returning an error to the caller");
                return refusal;
            }
            value => {
                if target.is_some() {
                    let refusal = Err(format!(
                        "`debug` inspects one repository, and {value:?} is a second path"
                    ));
                    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "parse_debug_args: returning an error to the caller");
                    return refusal;
                }
                target = Some(PathBuf::from(value));
            }
        }
    }
    Ok(DebugArgs { target, json })
}

/// Runs the debugger doctor and returns the report to render.
pub(crate) fn run_debug(args: &[String]) -> Result<DebugReport, String> {
    let request = parse_debug_args(args)?;
    let start = subject_path(request.target.as_deref());
    let root = repository_root(&start).map_err(|error| error.to_string())?;
    let mut config = lgwks_std::trace::DebugConfig::from_env("lgwks-deps")
        .map_err(|error| format!("cannot configure debugger: {error}"))?;
    if request.json {
        config = config.with_ansi(false);
    }
    let service_name = config.service_name().to_owned();
    let filter = config.filter().to_owned();
    let format = config.format();
    config
        .install()
        .map_err(|error| format!("cannot install debugger: {error}"))?;
    let surface = inspect_debug_surface(&root)?;
    let root_text = format!("{}", root.display());
    lgwks_std::trace::info!(
        service_name = service_name.as_str(),
        repository_root = root_text.as_str(),
        otel_schema_url = lgwks_std::trace::OTEL_SCHEMA_URL,
        default_debugger = surface.passes(),
        "debug doctor completed"
    );
    Ok(DebugReport {
        root,
        service_name,
        filter,
        format,
        surface,
        json: request.json,
    })
}

/// Inspects the standard-library manifest for the default debugger surface.
pub(crate) fn inspect_debug_surface(root: &Path) -> Result<DebugSurface, String> {
    let manifest_path = root.join("crates/lgwks-std/Cargo.toml");
    let manifest = std::fs::read_to_string(&manifest_path)
        .map_err(|error| format!("cannot read {}: {error}", manifest_path.display()))?;
    // A manifest that assigns no `default` or no `trace` list declares no
    // features there, so the question the surface answers is "does that list
    // include this entry" and an absent list answers it for itself.
    let default_value = assignment_value(&manifest, "default");
    let trace_value = assignment_value(&manifest, "trace");
    Ok(DebugSurface {
        default_includes_trace: default_value.is_some_and(|list| list.contains("\"trace\"")),
        trace_includes_tracing: trace_value
            .as_ref()
            .is_some_and(|list| list.contains("\"dep:tracing\"")),
        trace_includes_subscriber: trace_value
            .as_ref()
            .is_some_and(|list| list.contains("\"dep:tracing-subscriber\"")),
        tracing_declared: dependency_declared(&manifest, "tracing"),
        subscriber_declared: dependency_declared(&manifest, "tracing-subscriber"),
    })
}

/// Reads one TOML assignment value as text, including a multi-line array.
pub(crate) fn assignment_value(text: &str, key: &str) -> Option<String> {
    let prefix = format!("{key} =");
    let mut lines = text.lines().map(str::trim);
    while let Some(line) = lines.next() {
        let Some(value) = line.strip_prefix(&prefix) else {
            continue;
        };
        let mut collected = value.to_owned();
        let mut opens = value.matches('[').count();
        let mut closes = value.matches(']').count();
        while opens > closes {
            let Some(next) = lines.next() else {
                break;
            };
            collected.push(' ');
            collected.push_str(next);
            opens = opens.saturating_add(next.matches('[').count());
            closes = closes.saturating_add(next.matches(']').count());
        }
        return Some(collected);
    }
    None
}

/// True when a direct dependency assignment exists in a manifest.
pub(crate) fn dependency_declared(text: &str, name: &str) -> bool {
    let prefix = format!("{name} =");
    text.lines()
        .map(str::trim)
        .any(|line| line.starts_with(&prefix))
}

/// Handles the cargo-doctor-style debugger command.
pub(crate) fn handle_debug(
    args: &[String],
    out: &mut impl io::Write,
    err: &mut impl io::Write,
) -> io::Result<ExitCode> {
    let report = match run_debug(args) {
        Ok(report) => report,
        Err(message) => return refuse(&message, err),
    };
    if report.json {
        print_debug_json(&report, out)?;
    } else if report.surface.passes() {
        print_debug_human(&report, "OK", out)?;
    } else {
        print_debug_human(&report, "REFUSED", err)?;
    }
    Ok(if report.surface.passes() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(2)
    })
}

/// Prints the human debugger report.
pub(crate) fn print_debug_human(
    report: &DebugReport,
    status: &str,
    writer: &mut impl io::Write,
) -> io::Result<()> {
    writeln!(
        writer,
        "{status}  {} — lgwks_std debugger bootstrap is default-on",
        report.root.display()
    )?;
    writeln!(writer, "SERVICE  {}", report.service_name)?;
    writeln!(writer, "FILTER   {}", report.filter)?;
    writeln!(writer, "FORMAT   {}", report.format.as_str())?;
    writeln!(writer, "SCHEMA   {}", lgwks_std::trace::OTEL_SCHEMA_URL)?;
    debug_check_line(
        writer,
        report.surface.default_includes_trace,
        "default feature includes trace",
    )?;
    debug_check_line(
        writer,
        report.surface.trace_includes_tracing,
        "trace includes tracing facade",
    )?;
    debug_check_line(
        writer,
        report.surface.trace_includes_subscriber,
        "trace includes default subscriber",
    )?;
    debug_check_line(
        writer,
        report.surface.tracing_declared,
        "tracing dependency declared",
    )?;
    debug_check_line(
        writer,
        report.surface.subscriber_declared,
        "tracing-subscriber dependency declared",
    )
}

/// Prints one debugger check.
pub(crate) fn debug_check_line(
    writer: &mut impl io::Write,
    pass: bool,
    label: &str,
) -> io::Result<()> {
    let mark = if pass { "yes" } else { "no" };
    writeln!(writer, "CHECK    {mark:<3} {label}")
}

/// Prints the JSON debugger report.
pub(crate) fn print_debug_json(report: &DebugReport, out: &mut impl io::Write) -> io::Result<()> {
    let payload = lgwks_std::json::json!({
        "command": "debug",
        "admitted": report.surface.passes(),
        "root": format!("{}", report.root.display()),
        "service_name": report.service_name.as_str(),
        "filter": report.filter.as_str(),
        "format": report.format.as_str(),
        "otel": {
            "schema_url": lgwks_std::trace::OTEL_SCHEMA_URL
        },
        "checks": {
            "default_includes_trace": report.surface.default_includes_trace,
            "trace_includes_tracing": report.surface.trace_includes_tracing,
            "trace_includes_tracing_subscriber": report.surface.trace_includes_subscriber,
            "tracing_declared": report.surface.tracing_declared,
            "tracing_subscriber_declared": report.surface.subscriber_declared
        }
    });
    let rendered = lgwks_std::json::to_string_pretty(&payload).map_err(io::Error::other)?;
    writeln!(out, "{rendered}")
}
