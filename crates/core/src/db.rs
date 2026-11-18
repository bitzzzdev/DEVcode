//! SQLite access and bootstrap, mirroring packages/core/src/database/database.ts.
//! Fresh database: full initial schema plus the legacy migration ids recorded as
//! applied. Existing database (Bun-created): left untouched; the Bun build owns
//! the incremental migrations.

use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection};

use crate::initial_schema::{INITIAL_SCHEMA, LEGACY_MIGRATION_IDS};

#[derive(Debug)]
pub enum BootstrapError {
    Io(std::io::Error),
    Sqlite(rusqlite::Error),
    /// Database has user tables but no session table: same fatal condition the
    /// TypeScript build reports as "Database is not empty and has no session table".
    Foreign(Vec<String>),
}

impl std::fmt::Display for BootstrapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BootstrapError::Io(error) => write!(f, "{error}"),
            BootstrapError::Sqlite(error) => write!(f, "{error}"),
            BootstrapError::Foreign(tables) => {
                write!(
                    f,
                    "database is not empty and has no session table (found: {})",
                    tables.join(", ")
                )
            }
        }
    }
}

impl std::error::Error for BootstrapError {}

impl From<std::io::Error> for BootstrapError {
    fn from(error: std::io::Error) -> Self {
        BootstrapError::Io(error)
    }
}

impl From<rusqlite::Error> for BootstrapError {
    fn from(error: rusqlite::Error) -> Self {
        BootstrapError::Sqlite(error)
    }
}

/// DEVCODE_DB wins, the same override the Bun build honors for the CLI. Values are
/// resolved per packages/cli/src/database-path.ts: ":memory:" stays in-memory,
/// relative paths resolve against the XDG data directory, absolute paths are kept.
pub fn database_path(paths: &devcode_util::Paths) -> PathBuf {
    if let Some(override_path) = std::env::var_os("DEVCODE_DB") {
        let value = override_path.to_string_lossy().into_owned();
        if value == ":memory:" {
            return PathBuf::from(value);
        }
        let candidate = PathBuf::from(&value);
        return if candidate.is_absolute() {
            candidate
        } else {
            paths.data.join(value)
        };
    }
    let filename = if devcode_util::CHANNEL == "local" {
        format!("devcode-{}.db", devcode_util::CHANNEL)
    } else {
        "devcode.db".to_owned()
    };
    paths.data.join(filename)
}

#[derive(Debug)]
pub struct Database {
    pub conn: Connection,
}

impl Database {
    pub fn open(path: &Path) -> Result<Database, BootstrapError> {
        if path.to_str() != Some(":memory:") {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            if !path.exists() {
                // Create with 0600 before opening, like the Bun build's restrictToOwner.
                let file = fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
                }
                drop(file);
            }
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "busy_timeout", 5000)?;
        conn.pragma_update(None, "cache_size", -64000)?;
        conn.pragma_update(None, "foreign_keys", true)?;
        bootstrap(&conn)?;
        Ok(Database { conn })
    }
}

fn user_tables(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut statement = conn.prepare(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' AND substr(name, 1, 1) <> '_'",
    )?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    rows.collect()
}

fn bootstrap(conn: &Connection) -> Result<(), BootstrapError> {
    let tables = user_tables(conn)?;
    if tables
        .iter()
        .any(|table| table == "session" || table == "session_v2")
    {
        // Bun-created database (or already bootstrapped here): the Bun build owns
        // incremental migrations. Verify the bookkeeping table exists so a later
        // open does not mistake a hand-built file for a managed one.
        conn.execute_batch("CREATE TABLE IF NOT EXISTS migration (id TEXT PRIMARY KEY, time_completed INTEGER NOT NULL)")?;
        return Ok(());
    }
    if !tables.is_empty() {
        return Err(BootstrapError::Foreign(tables));
    }
    let tx = conn.unchecked_transaction()?;
    for statement in INITIAL_SCHEMA {
        tx.execute_batch(statement)?;
    }
    tx.execute_batch(
        "CREATE TABLE migration (id TEXT PRIMARY KEY, time_completed INTEGER NOT NULL)",
    )?;
    {
        let mut insert =
            tx.prepare("INSERT INTO migration (id, time_completed) VALUES (?1, ?2)")?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        for id in LEGACY_MIGRATION_IDS {
            insert.execute(params![id, now])?;
        }
    }
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "devcode-core-test-{tag}-{}",
            crate::identifier::ascending()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn fresh_database_gets_full_schema_and_legacy_ids() {
        let dir = temp_dir("fresh");
        let db = dir.join("devcode.db");
        {
            let handle = Database::open(&db).unwrap();
            let tables: Vec<String> = handle
                .conn
                .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
                .unwrap()
                .query_map([], |row| row.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            for expected in [
                "session_v2",
                "event",
                "event_sequence",
                "session_message",
                "session_part",
                "migration",
            ] {
                assert!(
                    tables.iter().any(|table| table == expected),
                    "missing table {expected}"
                );
            }
            let migrations: i64 = handle
                .conn
                .query_row("SELECT count(*) FROM migration", [], |row| row.get(0))
                .unwrap();
            assert_eq!(migrations as usize, LEGACY_MIGRATION_IDS.len());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&db).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        // Reopening an existing fresh database must not re-apply anything.
        let handle = Database::open(&db).unwrap();
        let migrations: i64 = handle
            .conn
            .query_row("SELECT count(*) FROM migration", [], |row| row.get(0))
            .unwrap();
        assert_eq!(migrations as usize, LEGACY_MIGRATION_IDS.len());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn database_with_foreign_tables_is_rejected() {
        let dir = temp_dir("foreign");
        let db = dir.join("devcode.db");
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch("CREATE TABLE something_else (id text primary key)")
                .unwrap();
        }
        let error = Database::open(&db).unwrap_err();
        assert!(
            matches!(error, BootstrapError::Foreign(tables) if tables == vec!["something_else".to_owned()])
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn database_path_honors_devcode_db_and_data_dir() {
        // Pure function: relative DEVCODE_DB joins the data dir, absolute stays, ":memory:" stays.
        let base = devcode_util::paths("devcode");
        std::env::set_var("DEVCODE_DB", "relative.db");
        assert_eq!(database_path(&base), base.data.join("relative.db"));
        let absolute = std::env::temp_dir().join("absolute.db");
        std::env::set_var("DEVCODE_DB", &absolute);
        assert_eq!(database_path(&base), absolute);
        std::env::remove_var("DEVCODE_DB");
        assert!(database_path(&base).ends_with("devcode.db"));
    }
}
