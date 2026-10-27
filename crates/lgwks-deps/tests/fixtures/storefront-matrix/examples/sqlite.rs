//! The estate's SQLite opens one way and migrates by version.
//!
//! This example is the journey the `sqlite` storefront feature sells: open
//! with the estate's defaults, migrate once, reopen and read the row back,
//! then refuse a migration past the held statements and a downgrade. It
//! exits non-zero on the first refusal, so the consumer test that runs it
//! distinguishes a working store from a broken one by the status alone.
//! Every failure surfaces as the exit status with no diagnostic output; the
//! consumer test prints the captured streams, so a broken store reads as a
//! failed journey, never as a crashed example.

use lgwks_deps::storefront::{rusqlite, sqlite};

fn main() {
    if !journey() {
        std::process::exit(1);
    }
}

fn journey() -> bool {
    let dir = std::env::temp_dir().join("lgwks-sqlite-journey");
    drop(std::fs::remove_dir_all(&dir));
    if std::fs::create_dir_all(&dir).is_err() {
        return false;
    }
    let path = dir.join("store.sqlite");

    // The engine travels through the storefront beside the helpers, so the
    // consumer names both and neither re-export rots beside the other.
    let memory = match rusqlite::Connection::open_in_memory() {
        Ok(connection) => connection,
        Err(_) => return false,
    };
    drop(memory);

    let connection = match sqlite::open(&path) {
        Ok(connection) => connection,
        Err(_) => return false,
    };
    let mode: String = match connection.pragma_query_value(None, "journal_mode", |row| row.get(0))
    {
        Ok(mode) => mode,
        Err(_) => return false,
    };
    if !mode.eq_ignore_ascii_case("wal") {
        return false;
    }
    if sqlite::migrate(
        &connection,
        1,
        &["CREATE TABLE flags(k TEXT PRIMARY KEY, v TEXT);"],
    )
    .is_err()
    {
        return false;
    }
    if connection
        .execute("INSERT INTO flags(k, v) VALUES ('b', '2')", [])
        .is_err()
    {
        return false;
    }
    drop(connection);

    let reopened = match sqlite::open(&path) {
        Ok(connection) => connection,
        Err(_) => return false,
    };
    let value: String =
        match reopened.query_row("SELECT v FROM flags WHERE k = 'b'", [], |row| row.get(0)) {
            Ok(value) => value,
            Err(_) => return false,
        };
    if value != "2" {
        return false;
    }
    let version: u32 =
        match reopened.pragma_query_value(None, "user_version", |row| row.get(0)) {
            Ok(version) => version,
            Err(_) => return false,
        };
    if version != 1 {
        return false;
    }
    match sqlite::migrate(
        &reopened,
        3,
        &["CREATE TABLE flags(k TEXT PRIMARY KEY, v TEXT);"],
    ) {
        Err(sqlite::SqliteError::Engine { .. }) => {}
        Err(_) => return false,
        Ok(_) => return false,
    }
    match sqlite::migrate(&reopened, 0, &[]) {
        Err(sqlite::SqliteError::Downgrade { .. }) => {}
        Err(_) => return false,
        Ok(_) => return false,
    }
    true
}
