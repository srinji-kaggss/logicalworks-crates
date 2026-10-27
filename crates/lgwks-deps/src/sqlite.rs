//! The estate's one durable store: SQLite opened one way, migrated by version.
//!
//! Every estate service that keeps state across runs opens it here instead of
//! declaring its own SQLite crate (#696): one pinned engine, WAL readers
//! beside a writer, kernel-released locks, a busy timeout that waits rather
//! than failing at once, and migrations recorded in `user_version` so a reopen
//! never replays history (#688). The engine itself is reached through
//! [`lgwks_deps::storefront::rusqlite`](crate::storefront::rusqlite); this
//! module is the opinion about how it is opened.

use std::fmt;
use std::path::Path;
use std::time::Duration;

use crate::storefront::rusqlite;

/// How long a busy store waits for another writer before it is refused.
///
/// Sixty seconds, the same bound the keyed run store waits under: a writer
/// holds the lock for one small transaction, so a waiter that outlives this
/// is queued behind a stuck holder rather than a live one, and the refusal
/// names the wait instead of hiding it.
///
/// Private on purpose: the bound is a behavioral contract, pinned by the
/// pragma a test reads back, not a vocabulary other crates coordinate on.
/// Services that wait on their own locks name their own bound.
const BUSY_TIMEOUT: Duration = Duration::from_secs(60);

/// Why the estate's SQLite refused.
#[derive(Debug)]
#[non_exhaustive]
pub enum SqliteError {
    /// The engine refused the open, a pragma, or a statement.
    Engine {
        /// What the engine said.
        cause: String,
    },
    /// The store is already past the migration the caller asked for.
    ///
    /// A downgrade is never run: the file stays exactly as it was, and the
    /// caller that wrote the newer version is the one that reads it.
    Downgrade {
        /// The `user_version` the file carries.
        found: u32,
        /// The version the caller asked for.
        wanted: u32,
    },
}

impl fmt::Display for SqliteError {
    /// The refusal, naming the engine's report or the two versions.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Engine { ref cause } => {
                write!(formatter, "the estate sqlite store refused: {cause}")
            }
            Self::Downgrade { found, wanted } => write!(
                formatter,
                "the store is at migration version {found} and the caller asked for {wanted}; \
                 downgrades never run"
            ),
        }
    }
}

impl std::error::Error for SqliteError {}

impl SqliteError {
    /// An engine refusal, carrying its report as text.
    ///
    /// One constructor because every pragma, open and statement below reports
    /// the same fact, and a literal per site would be several chances to name
    /// a different variant for it.
    fn engine(cause: impl fmt::Display) -> Self {
        Self::Engine {
            cause: cause.to_string(),
        }
    }
}

/// Open (or create) the SQLite store at `path` with the estate's defaults.
///
/// The file is created with its parent directories when absent. Every open
/// establishes the same four facts, so two services opening one file never
/// disagree about what they opened:
///
/// - `journal_mode = WAL`: readers run beside a writer, and a crash leaves a
///   write-ahead log the next open replays rather than a torn page.
/// - `synchronous = FULL`: a committed transaction survived power loss before
///   it was acknowledged.
/// - `busy_timeout` of [`BUSY_TIMEOUT`]: a waiter waits, then is told, and
///   nothing is half-written.
/// - `foreign_keys = ON`: a relationship the schema declares is one the
///   engine checks.
///
/// A journal mode the store did not take is a refusal, never a silent
/// fallback: running in delete mode where the caller was promised WAL readers
/// would serialize every read behind the writer (#688).
///
/// # Errors
///
/// [`SqliteError::Engine`] when the device, the engine, or a filesystem that
/// cannot carry a write-ahead log refuses.
#[must_use = "an unopened store holds no state"]
pub fn open(path: &Path) -> Result<rusqlite::Connection, SqliteError> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(SqliteError::engine)?;
    }
    let connection = rusqlite::Connection::open(path).map_err(SqliteError::engine)?;
    connection
        .pragma_update(None, "journal_mode", "WAL")
        .map_err(SqliteError::engine)?;
    let mode: String = connection
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .map_err(SqliteError::engine)?;
    if !mode.eq_ignore_ascii_case("wal") {
        lgwks_std::trace::warn!(
            mode = %mode,
            path = %path.display(),
            "estate sqlite: refusing a store that did not take WAL rather than running without WAL readers"
        );
        return Err(SqliteError::engine(format!(
            "journal_mode is {mode:?}, not WAL; refusing rather than running without WAL readers"
        )));
    }
    connection
        .pragma_update(None, "synchronous", "FULL")
        .map_err(SqliteError::engine)?;
    connection
        .pragma_update(
            None,
            "busy_timeout",
            i64::try_from(BUSY_TIMEOUT.as_millis()).map_err(SqliteError::engine)?,
        )
        .map_err(SqliteError::engine)?;
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .map_err(SqliteError::engine)?;
    Ok(connection)
}

/// Migrate an opened store to `target`, running each step once.
///
/// `migrations[i]` is the statement batch that moves the store from version
/// `i` to `i + 1`, so `target` must be at most `migrations.len()`. The version
/// the file carries is read from `user_version`; every step past it runs as
/// one batch, and `user_version` advances only after its step commits, so a
/// crash replays the unapplied tail rather than a half version. A store
/// already at `target` runs nothing and reports its version.
///
/// # Errors
///
/// [`SqliteError::Downgrade`] when the file is already past `target`;
/// [`SqliteError::Engine`] when a step, or the version read or write, is
/// refused. A refused step leaves the earlier steps committed and
/// `user_version` at the last one that did: the next open replays the tail.
pub fn migrate(
    connection: &rusqlite::Connection,
    target: u32,
    migrations: &[&str],
) -> Result<u32, SqliteError> {
    let found: u32 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(SqliteError::engine)?;
    if found > target {
        lgwks_std::trace::warn!(
            found,
            wanted = target,
            "estate sqlite: a downgrade never runs; the file stays as it was"
        );
        return Err(SqliteError::Downgrade {
            found,
            wanted: target,
        });
    }
    let steps = usize::try_from(target.saturating_sub(found)).map_err(SqliteError::engine)?;
    let have = usize::try_from(found).map_err(SqliteError::engine)?;
    if have.saturating_add(steps) > migrations.len() {
        lgwks_std::trace::warn!(
            target,
            steps,
            held = migrations.len(),
            "estate sqlite: a migration past the held statements is refused whole"
        );
        return Err(SqliteError::engine(format!(
            "migration to version {target} needs {} statements and holds {}",
            steps,
            migrations.len()
        )));
    }
    for (offset, statement) in migrations.iter().skip(have).take(steps).enumerate() {
        let version = found.saturating_add(u32::try_from(offset).map_err(SqliteError::engine)?);
        connection
            .execute_batch(statement)
            .map_err(SqliteError::engine)?;
        connection
            .pragma_update(None, "user_version", version.saturating_add(1))
            .map_err(SqliteError::engine)?;
    }
    Ok(target)
}

/// The `sqlite` command's argument parsing and reporting.
pub mod cli {
    use std::fmt;
    use std::path::{Path, PathBuf};

    use super::BUSY_TIMEOUT;
    use crate::storefront::rusqlite;

    /// Why `sqlite`'s arguments were refused.
    ///
    /// Refusals rather than guesses: a missing path, an unknown flag, or a
    /// target that is not a migration version each names the token that caused
    /// it.
    #[derive(Debug)]
    #[non_exhaustive]
    pub enum SqliteArgError {
        /// No store path was given.
        MissingPath,
        /// A flag this command does not define.
        UnknownFlag {
            /// The unrecognised token, verbatim.
            flag: String,
        },
        /// The `TARGET` that was not a migration version.
        InvalidTarget {
            /// The token it was handed instead.
            value: String,
        },
    }

    impl fmt::Display for SqliteArgError {
        /// The refusal, naming the token that caused it.
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            match *self {
                Self::MissingPath => write!(
                    formatter,
                    "`sqlite` opens one store: sqlite <PATH> [TARGET [SQL ...]]"
                ),
                Self::UnknownFlag { ref flag } => {
                    write!(formatter, "unknown option for `sqlite`: {flag}")
                }
                Self::InvalidTarget { ref value } => write!(
                    formatter,
                    "`sqlite` migrates to a whole version number, and {value:?} is not one"
                ),
            }
        }
    }

    impl std::error::Error for SqliteArgError {}

    /// What a `sqlite` invocation was asked to do.
    ///
    /// Fields stay private because `migrations` is mutable state: callers
    /// read the request through the accessors, and the only way to build one
    /// is the parser that validated it.
    #[derive(Debug)]
    #[non_exhaustive]
    pub struct SqliteArgs {
        /// The store to open with the estate's defaults.
        path: PathBuf,
        /// The migration version to reach, or `None` for a report only.
        target: Option<u32>,
        /// One statement batch per migration step past the stored version.
        migrations: Vec<String>,
    }

    impl SqliteArgs {
        /// The store to open with the estate's defaults.
        #[must_use]
        pub fn path(&self) -> &Path {
            &self.path
        }

        /// The migration version to reach, or `None` for a report only.
        #[must_use]
        pub const fn target(&self) -> Option<u32> {
            self.target
        }

        /// One statement batch per migration step past the stored version.
        #[must_use]
        pub fn migrations(&self) -> &[String] {
            &self.migrations
        }
    }

    /// What one parse of `sqlite`'s arguments resolved to.
    #[derive(Debug)]
    #[non_exhaustive]
    pub enum SqliteRequest {
        /// Open a store, migrate it when asked, and report it.
        Run(SqliteArgs),
        /// Print the usage block and exit successfully.
        Help,
    }

    /// Parses `sqlite`'s arguments in one consuming pass.
    ///
    /// The store path is required and comes first; the migration target, when
    /// present, is the whole version number that follows it, and every token past
    /// the target is one statement batch. There is no option to repeat, so a `-`
    /// token longer than one byte is an unknown flag, and a target that is not a
    /// version is refused rather than guessed at.
    pub fn parse_sqlite_args(args: &[String]) -> Result<SqliteRequest, SqliteArgError> {
        let mut positional: Vec<String> = Vec::new();
        for argument in args {
            match argument.as_str() {
                "--help" | "-h" => return Ok(SqliteRequest::Help),
                flag if flag.starts_with('-') && flag.len() > 1 => {
                    let refusal = Err(SqliteArgError::UnknownFlag {
                        flag: flag.to_owned(),
                    });
                    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "parse_sqlite_args: returning an error to the caller");
                    return refusal;
                }
                _ => positional.push(argument.clone()),
            }
        }
        let mut cursor = positional.into_iter();
        let Some(path) = cursor.next() else {
            let refusal = Err(SqliteArgError::MissingPath);
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "parse_sqlite_args: returning an error to the caller");
            return refusal;
        };
        let target = match cursor.next() {
            None => None,
            Some(text) => match text.parse::<u32>() {
                Ok(version) => Some(version),
                Err(_) => {
                    let refusal = Err(SqliteArgError::InvalidTarget { value: text });
                    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "parse_sqlite_args: returning an error to the caller");
                    return refusal;
                }
            },
        };
        Ok(SqliteRequest::Run(SqliteArgs {
            path: PathBuf::from(path),
            target,
            migrations: cursor.collect(),
        }))
    }

    /// Reports what the estate's defaults established for an opened store.
    ///
    /// Every line is one fact the next open can rely on: the recorded migration
    /// version, the journal mode readers run beside, the durability acknowledged,
    /// the wait before a busy store is refused, and how many tables the store
    /// holds.
    pub fn report(
        connection: &rusqlite::Connection,
        out: &mut impl std::io::Write,
    ) -> std::io::Result<()> {
        let version: u32 = match connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
        {
            Ok(version) => version,
            Err(error) => {
                lgwks_std::trace::debug!(error = ?error, "sqlite cli report: user_version unreadable");
                return Err(std::io::Error::other(error.to_string()));
            }
        };
        let mode: String = match connection
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
        {
            Ok(mode) => mode,
            Err(error) => {
                lgwks_std::trace::debug!(error = ?error, "sqlite cli report: journal_mode unreadable");
                return Err(std::io::Error::other(error.to_string()));
            }
        };
        let synchronous: i64 = match connection
            .pragma_query_value(None, "synchronous", |row| row.get(0))
        {
            Ok(level) => level,
            Err(error) => {
                lgwks_std::trace::debug!(error = ?error, "sqlite cli report: synchronous unreadable");
                return Err(std::io::Error::other(error.to_string()));
            }
        };
        let tables: i64 = match connection.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table'",
            [],
            |row| row.get(0),
        ) {
            Ok(count) => count,
            Err(error) => {
                lgwks_std::trace::debug!(error = ?error, "sqlite cli report: table count unreadable");
                return Err(std::io::Error::other(error.to_string()));
            }
        };
        writeln!(out, "user_version={version}")?;
        writeln!(out, "journal_mode={}", mode.to_ascii_lowercase())?;
        writeln!(out, "synchronous={synchronous}")?;
        writeln!(out, "busy_timeout_ms={}", BUSY_TIMEOUT.as_millis())?;
        writeln!(out, "tables={tables}")?;
        Ok(())
    }
}
