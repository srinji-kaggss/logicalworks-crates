//! Structured, levelled logging and default debugger bootstrap (feature
//! `trace`, default-on).
//!
//! This module exists because this workspace forbids `println!`/`eprintln!` in
//! library code and names `tracing` as the replacement, but `tracing` was
//! absent from every crate. A library author following that rule had nothing to
//! use, and one ignoring it had `println!`; the rule was enforceable against
//! the wrong practice and unenforceable in favour of the right one.
//!
//! It is a re-export, like the `json` module: this crate names the
//! capability rather than the crate, so replacing the implementation is one
//! change here rather than one per consumer.
//!
//! # What is here, and what is not
//!
//! The **macros, core types, and default subscriber installer** are here, which
//! is what calling code needs. The `attributes` feature is deliberately not
//! enabled (see the note in `Cargo.toml`), so `#[instrument]` is unavailable
//! from this path; it would pull `syn` into the foundation crate.
//!
//! `install_default` gives binaries and agents the no-decision path: compact
//! local output by default, `LGWKS_LOG` or `RUST_LOG` for filtering, and
//! `LGWKS_LOG_FORMAT=json` when the same stream needs to feed production
//! tooling. Libraries still only emit; applications install once at the edge.
//!
//! # Levels
//!
//! `ERROR` for a broken contract, `WARN` for degraded but serving, `INFO` for
//! lifecycle, `DEBUG` for developer diagnostics, `TRACE` for wire-level.
//!
//! # Example
//!
//! ```
//! use lgwks_std::trace::{info, warn};
//!
//! // A library emits. With no subscriber installed these are near-free no-ops;
//! // the binary that links this installs the subscriber that decides where
//! // records go.
//! info!(bytes = 512, "read a record");
//! warn!(remaining = 3, "retry budget is nearly spent");
//! ```
//!
//! Structured fields, not interpolated strings: a subscriber can filter on
//! `bytes` without parsing the message, which is the property `println!` cannot
//! provide, and the reason `println!` is banned in library code.

use std::env;
use std::error::Error;
use std::fmt;

pub use tracing::{self, Level};

pub use tracing::{debug, error, info, trace, warn};
pub use tracing::{debug_span, error_span, info_span, span, trace_span};
pub use tracing::{event, event_enabled};

pub use tracing::{Event, Span, Value};
// `Instrument` (the future combinator) is core; `#[instrument]` (the attribute)
// is not, because it lives behind the `attributes` feature this crate declines.
pub use tracing::Instrument;

/// Structured fields attached to an event or span.
///
/// Module rather than a re-export: call sites spell `field::Empty` and
/// `field::display`, so the path has to survive verbatim.
pub use tracing::field;

/// Primary filter environment variable read before `RUST_LOG`.
pub const LOG_FILTER_ENV: &str = "LGWKS_LOG";

/// Fallback filter environment variable used by Rust tracing tools.
pub const RUST_LOG_FILTER_ENV: &str = "RUST_LOG";

/// Output format environment variable for the default debugger.
pub const LOG_FORMAT_ENV: &str = "LGWKS_LOG_FORMAT";

/// Default filter when no filter environment variable is set.
pub const DEFAULT_FILTER: &str = "info";

/// Stable schema URL recorded by the debugger doctor and docs.
pub const OTEL_SCHEMA_URL: &str = "https://opentelemetry.io/schemas/1.27.0";

/// Subscriber output format for the default debugger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DebugFormat {
    /// One compact human-readable line per event.
    Compact,
    /// Expanded human-readable output for local inspection.
    Pretty,
    /// Structured JSON lines for production ingestion and agent parsing.
    Json,
}

impl DebugFormat {
    /// Reads [`LOG_FORMAT_ENV`], defaulting to compact output.
    pub fn from_env() -> Result<Self, DebugInstallError> {
        match env_string(LOG_FORMAT_ENV)? {
            Some(value) => Self::parse(&value).ok_or(DebugInstallError::InvalidFormat { value }),
            None => Ok(Self::Compact),
        }
    }

    /// Parses a format name.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        if value.eq_ignore_ascii_case("compact") {
            Some(Self::Compact)
        } else if value.eq_ignore_ascii_case("pretty") {
            Some(Self::Pretty)
        } else if value.eq_ignore_ascii_case("json") {
            Some(Self::Json)
        } else {
            None
        }
    }

    /// The stable name used by human and JSON doctor output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Compact => "compact",
            Self::Pretty => "pretty",
            Self::Json => "json",
        }
    }
}

/// Configuration for installing the default debugger subscriber.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct DebugConfig {
    /// OTel service name attached to lifecycle events and doctor output.
    service_name: String,
    /// `tracing_subscriber::EnvFilter` directive string.
    filter: String,
    /// Output format.
    format: DebugFormat,
    /// Whether ANSI colour escapes are allowed.
    ansi: bool,
    /// Whether event targets are printed.
    targets: bool,
    /// Whether thread names are printed.
    thread_names: bool,
    /// Whether thread ids are printed.
    thread_ids: bool,
}

impl DebugConfig {
    /// Builds a config with the default filter and compact local output.
    #[must_use]
    pub fn new(service_name: impl Into<String>) -> Self {
        Self {
            service_name: service_name.into(),
            filter: DEFAULT_FILTER.to_owned(),
            format: DebugFormat::Compact,
            ansi: true,
            targets: true,
            thread_names: true,
            thread_ids: false,
        }
    }

    /// Builds a config from `LGWKS_LOG`, `RUST_LOG`, and `LGWKS_LOG_FORMAT`.
    pub fn from_env(service_name: impl Into<String>) -> Result<Self, DebugInstallError> {
        let filter = filter_from_env()?;
        let format = DebugFormat::from_env()?;
        Ok(Self::new(service_name)
            .with_filter(filter)
            .with_format(format))
    }

    /// Overrides the filter directive.
    #[must_use]
    pub fn with_filter(mut self, filter: impl Into<String>) -> Self {
        self.filter = filter.into();
        self
    }

    /// Overrides the output format.
    #[must_use]
    pub const fn with_format(mut self, format: DebugFormat) -> Self {
        self.format = format;
        self
    }

    /// Enables or disables ANSI colour output.
    #[must_use]
    pub const fn with_ansi(mut self, ansi: bool) -> Self {
        self.ansi = ansi;
        self
    }

    /// Enables or disables target rendering.
    #[must_use]
    pub const fn with_targets(mut self, targets: bool) -> Self {
        self.targets = targets;
        self
    }

    /// Enables or disables thread-name rendering.
    #[must_use]
    pub const fn with_thread_names(mut self, thread_names: bool) -> Self {
        self.thread_names = thread_names;
        self
    }

    /// Enables or disables thread-id rendering.
    #[must_use]
    pub const fn with_thread_ids(mut self, thread_ids: bool) -> Self {
        self.thread_ids = thread_ids;
        self
    }

    /// The service name this debugger config will report.
    #[must_use]
    pub fn service_name(&self) -> &str {
        &self.service_name
    }

    /// The filter directive this debugger config will install.
    #[must_use]
    pub fn filter(&self) -> &str {
        &self.filter
    }

    /// The output format this debugger config will install.
    #[must_use]
    pub const fn format(&self) -> DebugFormat {
        self.format
    }

    /// Installs this config as the process-global tracing subscriber.
    pub fn install(self) -> Result<(), DebugInstallError> {
        self.validate()?;
        let filter = tracing_subscriber::EnvFilter::try_new(&self.filter).map_err(|source| {
            DebugInstallError::InvalidFilter {
                value: self.filter.clone(),
                source,
            }
        })?;
        let service_name = self.service_name.clone();
        let format = self.format;
        let builder = tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .with_env_filter(filter)
            .with_ansi(self.ansi)
            .with_target(self.targets)
            .with_thread_names(self.thread_names)
            .with_thread_ids(self.thread_ids);
        let subscriber: Box<dyn tracing::Subscriber + Send + Sync> = match format {
            DebugFormat::Compact => Box::new(builder.compact().finish()),
            DebugFormat::Pretty => Box::new(builder.pretty().finish()),
            DebugFormat::Json => Box::new(builder.json().flatten_event(true).finish()),
        };
        tracing::subscriber::set_global_default(subscriber)
            .map_err(|source| DebugInstallError::InstallFailed { source })?;
        info!(
            service_name = service_name.as_str(),
            otel_schema_url = OTEL_SCHEMA_URL,
            debugger_format = format.as_str(),
            "debugger installed"
        );
        Ok(())
    }

    /// Refuses a config that cannot identify the emitting service.
    fn validate(&self) -> Result<(), DebugInstallError> {
        if self.service_name.trim().is_empty() {
            let refusal = Err(DebugInstallError::MissingServiceName);
            #[cfg(feature = "trace")]
            crate::trace::debug!(error = ?refusal.as_ref().err(), "validate: returning an error to the caller");
            return refusal;
        }
        Ok(())
    }
}

/// Installs the default debugger from process environment.
pub fn install_default(service_name: impl Into<String>) -> Result<(), DebugInstallError> {
    DebugConfig::from_env(service_name)?.install()
}

/// Errors returned while installing the default debugger.
#[derive(Debug)]
#[non_exhaustive]
pub enum DebugInstallError {
    /// The caller did not name the application or service.
    MissingServiceName,
    /// An environment variable was present but not valid Unicode.
    InvalidEnvironment {
        /// Environment variable that could not be read as Unicode.
        variable: &'static str,
        /// Original environment error.
        source: env::VarError,
    },
    /// `LGWKS_LOG_FORMAT` named no supported output format.
    InvalidFormat {
        /// Invalid format value.
        value: String,
    },
    /// The filter directive could not be parsed.
    InvalidFilter {
        /// Invalid filter directive.
        value: String,
        /// Original filter parse error.
        source: tracing_subscriber::filter::ParseError,
    },
    /// A process-global subscriber was already installed.
    InstallFailed {
        /// Original global-install failure.
        source: tracing::subscriber::SetGlobalDefaultError,
    },
}

impl fmt::Display for DebugInstallError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::MissingServiceName => write!(formatter, "debugger service name is empty"),
            Self::InvalidEnvironment { variable, .. } => {
                write!(formatter, "{variable} is not valid Unicode")
            }
            Self::InvalidFormat { ref value } => write!(
                formatter,
                "{LOG_FORMAT_ENV} value {value:?} is not one of compact, pretty, json"
            ),
            Self::InvalidFilter { ref value, .. } => {
                write!(formatter, "trace filter {value:?} is invalid")
            }
            Self::InstallFailed { .. } => {
                write!(
                    formatter,
                    "a global tracing subscriber is already installed"
                )
            }
        }
    }
}

impl Error for DebugInstallError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match *self {
            Self::InvalidEnvironment { ref source, .. } => Some(source),
            Self::InvalidFilter { ref source, .. } => Some(source),
            Self::InstallFailed { ref source } => Some(source),
            Self::MissingServiceName | Self::InvalidFormat { .. } => None,
        }
    }
}

/// Reads an environment variable as an optional non-empty string.
fn env_string(variable: &'static str) -> Result<Option<String>, DebugInstallError> {
    match env::var(variable) {
        Ok(value) if value.trim().is_empty() => Ok(None),
        Ok(value) => Ok(Some(value)),
        Err(env::VarError::NotPresent) => Ok(None),
        Err(source) => Err(DebugInstallError::InvalidEnvironment { variable, source }),
    }
}

/// The filter directive this crate installs: the primary variable, then the
/// standard one, then the documented default.
///
/// Each source is read with its own refusal propagated — a variable that is
/// present but not valid Unicode is an error naming which variable it was —
/// rather than one read standing in for the other two.
fn filter_from_env() -> Result<String, DebugInstallError> {
    match env_string(LOG_FILTER_ENV)? {
        Some(primary) => Ok(primary),
        None => match env_string(RUST_LOG_FILTER_ENV)? {
            Some(standard) => Ok(standard),
            None => Ok(DEFAULT_FILTER.to_owned()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{DebugConfig, DebugFormat, DebugInstallError, LOG_FORMAT_ENV};

    /// Shared test result type.
    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn format_parser_accepts_the_documented_names() {
        assert_eq!(
            DebugFormat::parse("compact"),
            Some(DebugFormat::Compact),
            "compact format should parse"
        );
        assert_eq!(
            DebugFormat::parse("PRETTY"),
            Some(DebugFormat::Pretty),
            "format names are case-insensitive"
        );
        assert_eq!(
            DebugFormat::parse("json"),
            Some(DebugFormat::Json),
            "json format should parse"
        );
        assert_eq!(
            DebugFormat::parse("yaml"),
            None,
            "undocumented formats are refused"
        );
    }

    #[test]
    fn config_refuses_an_empty_service_name() {
        let error = DebugConfig::new("   ").install().err();
        assert!(
            matches!(error, Some(DebugInstallError::MissingServiceName)),
            "empty service names must not install a nameless telemetry source: {error:?}"
        );
    }

    #[test]
    fn invalid_filter_is_reported_as_filter_error() -> TestResult {
        let error = DebugConfig::new("filter-test")
            .with_filter("[")
            .install()
            .err()
            .ok_or("invalid filter unexpectedly installed")?;
        assert!(
            matches!(error, DebugInstallError::InvalidFilter { .. }),
            "invalid filters should fail before global subscriber install: {error:?}"
        );
        Ok(())
    }

    #[test]
    fn format_names_are_stable_for_the_doctor() {
        assert_eq!(
            DebugFormat::Json.as_str(),
            "json",
            "doctor output should use stable format names"
        );
        assert_eq!(
            LOG_FORMAT_ENV, "LGWKS_LOG_FORMAT",
            "format environment variable is the documented one"
        );
    }
}
