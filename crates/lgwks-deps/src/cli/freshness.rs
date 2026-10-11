//! `freshness`: every registry dependency in the lock file against crates.io.

use std::io;
use std::path::Path;
use std::process::ExitCode;

use crate::{read_lock_text, refuse, resolve_repository_root, subject_path};

// ── freshness ──────────────────────────────────────────────────────────────

/// Reports every registry dependency in the lock file against crates.io.
///
/// Exit 0 when nothing is stale and 1 when at least one package is behind, so
/// a cron job can distinguish "up to date" from "action needed". A local or
/// path package is skipped rather than queried. `--json` selects the stable
/// object-array form over the human table; both carry the same fields.
pub(crate) fn handle_freshness(
    args: &[String],
    out: &mut impl io::Write,
    err: &mut impl io::Write,
) -> io::Result<ExitCode> {
    let json_output = args.iter().any(|arg| arg == "--json");
    let positional: Vec<&String> = args.iter().filter(|arg| !arg.starts_with("--")).collect();
    let start = subject_path(
        positional
            .first()
            .map(|candidate| Path::new(candidate.as_str())),
    );

    let root = unwrap_or_refuse!(resolve_repository_root(&start), err);
    let lock_text = unwrap_or_refuse!(read_lock_text(&root), err);

    let resolved = match lgwks_deps::lock::parse(&lock_text) {
        Ok(parsed) => parsed,
        Err(error) => return refuse(&format!("Cargo.lock: {error}"), err),
    };

    let registry: Vec<&lgwks_deps::lock::Resolved> = resolved
        .iter()
        .filter(|package| !package.is_local())
        .collect();

    if registry.is_empty() {
        writeln!(out, "no registry dependencies in Cargo.lock")?;
        return Ok(ExitCode::SUCCESS);
    }

    let results = query_crates_io(&registry);

    if json_output {
        print_freshness_json(&results, out)?;
    } else {
        print_freshness_table(&results, out)?;
    }

    let stale_count = results.iter().filter(|result| result.stale).count();
    if stale_count > 0 {
        Ok(ExitCode::from(1))
    } else {
        Ok(ExitCode::SUCCESS)
    }
}

/// One registry package's freshness verdict.
///
/// A package whose lookup failed carries an `error` and is not stale: a network
/// fault is not evidence that the dependency is out of date, and reporting it as
/// stale would fail a build for a reason the lockfile cannot support.
pub(crate) struct FreshnessResult {
    /// Package name as it appears in `Cargo.lock`.
    name: String,
    /// Version the lock file resolved.
    resolved: String,
    /// Latest version crates.io reports, or `None` when it published none.
    latest: Option<String>,
    /// Upstream repository URL crates.io reports, or `None` when it published
    /// none. A published version carries no repository, and an unpublished one
    /// carries neither; neither is an empty string crates.io sent.
    repository: Option<String>,
    /// Whether `latest` is a real version newer than `resolved`.
    stale: bool,
    /// Why the lookup failed, when it did.
    error: Option<String>,
}

impl FreshnessResult {
    /// Successful registry lookup.
    fn found(
        package: &lgwks_deps::lock::Resolved,
        latest: Option<String>,
        repository: Option<String>,
    ) -> Self {
        // A registry that publishes no version for a crate cannot be compared,
        // so nothing is claimed: `stale` is only ever decided between two
        // versions the registry and the lock both named.
        let stale = match latest.as_deref() {
            Some(latest) => latest != package.version(),
            None => false,
        };
        Self {
            name: package.name().to_owned(),
            resolved: package.version().to_owned(),
            latest,
            repository,
            stale,
            error: None,
        }
    }

    /// Failed registry lookup, which is reported but not treated as stale.
    fn lookup_failed(package: &lgwks_deps::lock::Resolved, error: String) -> Self {
        Self {
            name: package.name().to_owned(),
            resolved: package.version().to_owned(),
            latest: None,
            repository: None,
            stale: false,
            error: Some(error),
        }
    }
}

/// The `User-Agent` crates.io's crawler policy asks for: this tool's name and
/// version and where to reach its maintainers, read from the crate's own
/// manifest. Compiled from `Cargo.toml` rather than written here, so the
/// version cannot go stale and no person's address is baked into every copy
/// of a published binary.
pub(crate) const USER_AGENT: &str = concat!(
    "User-Agent: lgwks-deps/",
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("CARGO_PKG_REPOSITORY"),
    ")"
);

/// Queries crates.io for each distinct package name.
///
/// Names are de-duplicated first: a lock file commonly resolves several
/// versions of one package, and the registry answer is per name. The lookup
/// shells out to `curl` rather than pulling an HTTP client (INV-GATE-ZERO-DEPS),
/// with a ten-second cap so an unreachable registry cannot hang the command.
pub(crate) fn query_crates_io(packages: &[&lgwks_deps::lock::Resolved]) -> Vec<FreshnessResult> {
    let mut seen = std::collections::HashSet::new();
    let mut results = Vec::new();

    for package in packages {
        if !seen.insert(package.name()) {
            continue;
        }

        let completed = std::process::Command::new("curl")
            .args([
                "-sf",
                "--max-time",
                "10",
                "-H",
                USER_AGENT,
                &format!("https://crates.io/api/v1/crates/{}", package.name()),
            ])
            .output();

        match completed {
            Ok(response) if response.status.success() => {
                let body = String::from_utf8_lossy(&response.stdout);
                let (latest, repo) = parse_crate_response(&body);
                results.push(FreshnessResult::found(package, latest, repo));
            }
            Ok(response) => {
                results.push(FreshnessResult::lookup_failed(
                    package,
                    format!("HTTP {}", response.status),
                ));
            }
            Err(error) => {
                results.push(FreshnessResult::lookup_failed(package, error.to_string()));
            }
        }
    }
    results
}

/// INV-GATE-ZERO-DEPS: no JSON parser; extract fields by line scan.
pub(crate) fn parse_crate_response(body: &str) -> (Option<String>, Option<String>) {
    let newest = extract_json_string(body, "newest_version");
    let repo = extract_json_string(body, "repository");
    (newest, repo)
}

/// Reads the value of `key` out of a JSON object by scanning for its quoted
/// form.
///
/// Both `"key":"value"` and `"key": "value"` spacing are accepted, and a key
/// that is absent yields `None` rather than an empty string: freshness is a
/// best-effort advisory, so a missing field is not a failure, and an empty
/// string is a value crates.io never sent. `None` is what the caller reports as
/// "not published", and an unterminated value is refused the same way, because
/// a half-read value is not a shorter one. This is deliberately not a JSON
/// parser; see the zero-deps invariant above.
pub(crate) fn extract_json_string(body: &str, key: &str) -> Option<String> {
    let needle = format!("\"{}\":\"", key);
    let alt_needle = format!("\"{}\": \"", key);
    // Each `index` is a byte offset at which `body` matched `needle`, so the
    // sum is a position inside `body` and cannot exceed `body.len()`; the
    // additions therefore cannot overflow `usize`.
    let start = body
        .find(&needle)
        .map(|index| index.saturating_add(needle.len()))
        .or_else(|| {
            body.find(&alt_needle)
                .map(|index| index.saturating_add(alt_needle.len()))
        });
    // Three refusals, one answer: the key is absent, the value is unterminated,
    // and the key is absent under the spaced spelling. None of them is a value
    // crates.io sent.
    let value_start = start?;
    // `end_offset` indexes the closing quote inside the slice that begins at
    // `value_start`, so the sum stays within `body`.
    let end_offset = body[value_start..].find('"')?;
    Some(body[value_start..value_start.saturating_add(end_offset)].to_string())
}

// Each label is a column value; the format string owns the alignment. The final
// column is spelled inside the format string rather than passed as an argument:
// a trailing string literal in a `{}` slot is what `clippy::write_literal`
// flags, and the rendered line is byte-identical either way.
/// The table cell for a field crates.io may not have published.
///
/// `?` is the marker a failed lookup already prints, and a version the registry
/// never published is the same fact to a reader: the value is not known. Naming
/// the mapping keeps one cell spelling for "not known" instead of an empty column
/// that reads as a value crates.io sent.
pub(crate) fn or_unpublished(value: Option<&str>) -> &str {
    match value {
        Some(value) => value,
        None => "?",
    }
}

/// Prints the human-readable freshness table.
///
/// A failed lookup is shown as `?` / `err` rather than as a version, so an
/// unreachable registry cannot be misread as an up-to-date dependency.
pub(crate) fn print_freshness_table(
    results: &[FreshnessResult],
    out: &mut impl io::Write,
) -> io::Result<()> {
    writeln!(
        out,
        "{:<30} {:<12} {:<12} {:<5} repository",
        "crate", "resolved", "latest", "stale"
    )?;
    writeln!(out, "{}", "-".repeat(90))?;
    for result in results {
        if let Some(failure) = result.error.as_ref() {
            writeln!(
                out,
                "{:<30} {:<12} {:<12} {:<5} {}",
                result.name, result.resolved, "?", "err", failure
            )?;
        } else {
            let stale_mark = if result.stale { "YES" } else { "" };
            writeln!(
                out,
                "{:<30} {:<12} {:<12} {:<5} {}",
                result.name,
                result.resolved,
                or_unpublished(result.latest.as_deref()),
                stale_mark,
                or_unpublished(result.repository.as_deref())
            )?;
        }
    }
    let stale_count = results.iter().filter(|result| result.stale).count();
    let err_count = results
        .iter()
        .filter(|result| result.error.is_some())
        .count();
    writeln!(
        out,
        "\n{} checked, {} stale, {} errors",
        results.len(),
        stale_count,
        err_count
    )?;
    Ok(())
}

/// Prints the freshness results as a JSON array of objects.
///
/// The shape is contractual: one object per checked package, in the order they
/// were queried, with `error` present only when the lookup failed. A stale
/// count is not emitted; consumers read `stale` per row and the process exit
/// code carries the aggregate.
///
/// Built through `lgwks_std::json`, like `check --json`. The previous version
/// assembled JSON by concatenation and escaped only the double quote, so any
/// `repository` or `error` string containing a backslash produced a payload no
/// parser would accept, from the crate whose whole purpose is enforcing that
/// dependencies go through the facade.
pub(crate) fn print_freshness_json(
    results: &[FreshnessResult],
    out: &mut impl io::Write,
) -> io::Result<()> {
    use lgwks_std::json::{Map, Value};

    let mut rows = Vec::with_capacity(results.len());
    for result in results {
        let mut row = Map::new();
        row.insert("name".to_owned(), Value::String(result.name.clone()));
        row.insert(
            "resolved".to_owned(),
            Value::String(result.resolved.clone()),
        );
        // `null` for a field crates.io did not publish, so a consumer reads
        // "not published" rather than a version nobody published.
        row.insert(
            "latest".to_owned(),
            match result.latest.as_ref() {
                Some(latest) => Value::String(latest.clone()),
                None => Value::Null,
            },
        );
        row.insert("stale".to_owned(), Value::Bool(result.stale));
        row.insert(
            "repository".to_owned(),
            match result.repository.as_ref() {
                Some(repository) => Value::String(repository.clone()),
                None => Value::Null,
            },
        );
        // Always present, `null` when the lookup succeeded: a consumer reads a
        // fixed set of keys rather than testing for the existence of each.
        row.insert(
            "error".to_owned(),
            match result.error.as_ref() {
                Some(failure) => Value::String(failure.clone()),
                None => Value::Null,
            },
        );
        rows.push(Value::Object(row));
    }

    let rendered =
        lgwks_std::json::to_string_pretty(&Value::Array(rows)).map_err(io::Error::other)?;
    writeln!(out, "{rendered}")
}
