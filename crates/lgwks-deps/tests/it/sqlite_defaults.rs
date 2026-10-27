//! The estate's SQLite opens one way and migrates by version (#696, #700).
//!
//! Every service that keeps state across runs takes its durable store through
//! [`lgwks_deps::sqlite`](lgwks_deps::sqlite) instead of declaring its own
//! SQLite crate: one pinned engine, WAL readers beside a writer,
//! kernel-released locks, a busy timeout, and `user_version` migrations. Every
//! fact here is proved against the real engine on a real file — the pragma
//! set is read back, a row survives a reopen, and a migration runs once.

#![cfg(feature = "sqlite")]

use lgwks_deps::rusqlite::Connection;
use lgwks_deps::sqlite::{SqliteError, migrate, open};
use lgwks_deps::storefront::{rusqlite as storefront_engine, sqlite as storefront_sqlite};

use crate::lock_fixture::Scratch;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn pragma_i64(connection: &Connection, name: &str) -> Result<i64, Box<dyn std::error::Error>> {
    Ok(connection.pragma_query_value(None, name, |row| row.get(0))?)
}

/// The engine is reachable through both documented paths, so neither
/// re-export rots beside the other. The busy bound itself is asserted where
/// it takes effect: the `busy_timeout` pragma a store opens with.
fn both_paths_name_the_same_engine() {
    let _root: fn() -> Result<Connection, storefront_engine::Error> =
        storefront_engine::Connection::open_in_memory;
    let _storefront: fn() -> Result<storefront_engine::Connection, storefront_engine::Error> =
        Connection::open_in_memory;
    let _helpers: fn(&std::path::Path) -> Result<storefront_engine::Connection, SqliteError> =
        storefront_sqlite::open;
}

#[test]
fn open_establishes_the_estates_defaults() -> TestResult {
    both_paths_name_the_same_engine();
    let scratch = Scratch::new("defaults")?;
    let connection = open(&scratch.path().join("store.sqlite"))?;
    let mode: String = connection.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
    assert_eq!(mode.to_lowercase(), "wal", "readers run beside a writer");
    assert_eq!(
        pragma_i64(&connection, "synchronous")?,
        2,
        "a commit survives power loss before it is acknowledged"
    );
    // The estate default, pinned where it takes effect: the module keeps the
    // constant private on purpose, so this literal is the contract a change
    // to the bound must update beside it.
    assert_eq!(
        pragma_i64(&connection, "busy_timeout")?,
        60_000i64,
        "a waiter waits the estate bound, then is told"
    );
    assert_eq!(
        pragma_i64(&connection, "foreign_keys")?,
        1,
        "a declared relationship is checked"
    );
    connection.execute_batch("CREATE TABLE kv(k TEXT PRIMARY KEY, v TEXT);")?;
    connection.execute("INSERT INTO kv(k, v) VALUES ('a', '1')", [])?;
    drop(connection);
    let reopened = open(&scratch.path().join("store.sqlite"))?;
    let value: String =
        reopened.query_row("SELECT v FROM kv WHERE k = 'a'", [], |row| row.get(0))?;
    assert_eq!(value, "1", "a committed row survives a reopen");
    Ok(())
}

#[test]
fn migrations_run_once_and_never_twice() -> TestResult {
    let scratch = Scratch::new("migrate")?;
    let connection = open(&scratch.path().join("store.sqlite"))?;
    let migrations = [
        "CREATE TABLE kv(k TEXT PRIMARY KEY, v TEXT);",
        "ALTER TABLE kv ADD COLUMN w TEXT;",
    ];
    assert_eq!(
        migrate(&connection, 2, &migrations)?,
        2,
        "two steps reach version two"
    );
    let version: u32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    assert_eq!(version, 2, "the version is recorded, not recomputed");
    connection.execute("INSERT INTO kv(k, v, w) VALUES ('a', '1', 'x')", [])?;
    assert_eq!(
        migrate(&connection, 2, &migrations)?,
        2,
        "a store already at its version runs nothing"
    );
    let kept: String =
        connection.query_row("SELECT w FROM kv WHERE k = 'a'", [], |row| row.get(0))?;
    assert_eq!(kept, "x", "a no-op migration moves no row");
    match migrate(&connection, 3, &migrations) {
        Err(SqliteError::Engine { .. }) => Ok::<(), Box<dyn std::error::Error>>(()),
        Err(other) => {
            return Err(
                format!("a version past the statements must be Engine, got {other}").into(),
            );
        }
        Ok(_) => return Err("a version past the statements must not migrate".into()),
    }?;
    match migrate(&connection, 1, &migrations) {
        Err(SqliteError::Downgrade { found, wanted }) => {
            assert_eq!(found, 2, "the refusal names the version on disk");
            assert_eq!(wanted, 1, "the refusal names the version asked for");
            Ok::<(), Box<dyn std::error::Error>>(())
        }
        Err(other) => {
            return Err(format!("a downgrade must be Downgrade, got {other}").into());
        }
        Ok(_) => return Err("a downgrade must never run".into()),
    }?;
    let version: u32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    assert_eq!(
        version, 2,
        "a refused migration leaves the recorded version where it was"
    );
    Ok(())
}
