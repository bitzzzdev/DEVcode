//! Durable per-aggregate event log, mirroring the storage half of
//! packages/core/src/bus.ts and packages/core/src/event/sql.ts.
//! Schema is owned by the `event` and `event_sequence` tables. Appends reserve
//! the next sequence inside a transaction; reads replay the ordered log.

use rusqlite::{params, Connection, OptionalExtension};

use crate::identifier;

#[derive(Debug, Clone, PartialEq)]
pub struct SerializedEvent {
    pub id: String,
    pub r#type: String,
    pub created: i64,
    pub seq: i64,
    pub aggregate_id: String,
    pub data: serde_json::Value,
}

#[derive(Debug)]
pub enum AppendError {
    Sqlite(rusqlite::Error),
    /// The aggregate log was reserved past the sequence we computed; another
    /// writer raced us. Callers re-read and retry, matching reserveSequence's
    /// max() semantics in the TypeScript build.
    Conflict,
}

impl std::fmt::Display for AppendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppendError::Sqlite(error) => write!(f, "{error}"),
            AppendError::Conflict => write!(f, "event sequence conflict"),
        }
    }
}

impl std::error::Error for AppendError {}

impl From<rusqlite::Error> for AppendError {
    fn from(error: rusqlite::Error) -> Self {
        AppendError::Sqlite(error)
    }
}

pub fn latest_sequence(conn: &Connection, aggregate_id: &str) -> rusqlite::Result<i64> {
    let row: Option<i64> = conn
        .query_row(
            "SELECT seq FROM event_sequence WHERE aggregate_id = ?1",
            params![aggregate_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(row.unwrap_or(-1))
}

/// Reserve `seq` for the aggregate: keep max(existing, seq). Mirrors
/// Bus.reserveSequence; used by both appends and forked-aggregate inheritance.
pub fn reserve_sequence(conn: &Connection, aggregate_id: &str, seq: i64) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO event_sequence (aggregate_id, seq) VALUES (?1, ?2) \
         ON CONFLICT(aggregate_id) DO UPDATE SET seq = max(seq, excluded.seq)",
        params![aggregate_id, seq],
    )?;
    Ok(())
}

/// Append one durable event. Returns the committed sequence number.
/// `created` is milliseconds since the epoch; `owner` maps to event_sequence.owner_id.
pub fn append_durable(
    conn: &mut Connection,
    aggregate_id: &str,
    event_type: &str,
    data: &serde_json::Value,
    created: i64,
    owner: Option<&str>,
) -> Result<SerializedEvent, AppendError> {
    let tx = conn.transaction()?;
    let seq = latest_sequence(&tx, aggregate_id)? + 1;
    reserve_sequence(&tx, aggregate_id, seq)?;
    if let Some(owner) = owner {
        tx.execute(
            "UPDATE event_sequence SET owner_id = ?2 WHERE aggregate_id = ?1",
            params![aggregate_id, owner],
        )?;
    }
    let id = format!("evt_{}", identifier::ascending());
    tx.execute(
        "INSERT INTO event (id, aggregate_id, seq, created, type, data) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![id, aggregate_id, seq, created, event_type, data.to_string()],
    )?;
    tx.commit()?;
    Ok(SerializedEvent {
        id,
        r#type: event_type.to_owned(),
        created,
        seq,
        aggregate_id: aggregate_id.to_owned(),
        data: data.clone(),
    })
}

/// Read the ordered durable log for an aggregate. `after` is exclusive; -1 reads from the start.
pub fn read_log(
    conn: &Connection,
    aggregate_id: &str,
    after: i64,
    limit: usize,
) -> rusqlite::Result<Vec<SerializedEvent>> {
    let mut statement = conn.prepare(
        "SELECT id, type, created, seq, data FROM event WHERE aggregate_id = ?1 AND seq > ?2 ORDER BY seq ASC LIMIT ?3",
    )?;
    let rows = statement.query_map(params![aggregate_id, after, limit as i64], |row| {
        let data: String = row.get(4)?;
        Ok(SerializedEvent {
            id: row.get(0)?,
            r#type: row.get(1)?,
            created: row.get(2)?,
            seq: row.get(3)?,
            aggregate_id: aggregate_id.to_owned(),
            data: serde_json::from_str(&data).unwrap_or(serde_json::Value::Null),
        })
    })?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use std::path::PathBuf;

    fn temp_db() -> (PathBuf, Database) {
        let dir =
            std::env::temp_dir().join(format!("devcode-event-test-{}", identifier::ascending()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("devcode.db");
        let database = Database::open(&db_path).unwrap();
        (dir, database)
    }

    #[test]
    fn appends_reserve_sequential_sequences_and_replay_in_order() {
        let (dir, database) = temp_db();
        let mut conn = database.conn;
        let first = append_durable(
            &mut conn,
            "session123",
            "session.created",
            &serde_json::json!({"sessionID": "session123"}),
            1000,
            None,
        )
        .unwrap();
        let second = append_durable(
            &mut conn,
            "session123",
            "session.renamed",
            &serde_json::json!({"sessionID": "session123", "title": "x"}),
            2000,
            None,
        )
        .unwrap();
        assert_eq!((first.seq, second.seq), (0, 1));
        assert!(first.id.starts_with("evt_"));
        assert_ne!(first.id, second.id);

        let log = read_log(&conn, "session123", -1, 512).unwrap();
        assert_eq!(
            log.iter().map(|event| event.seq).collect::<Vec<_>>(),
            vec![0, 1]
        );
        assert_eq!(log[1].r#type, "session.renamed");
        assert_eq!(log[1].data["title"], "x");

        let tail = read_log(&conn, "session123", 0, 512).unwrap();
        assert_eq!(
            tail.iter().map(|event| event.seq).collect::<Vec<_>>(),
            vec![1]
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn separate_aggregates_keep_independent_sequences() {
        let (dir, database) = temp_db();
        let mut conn = database.conn;
        let a = append_durable(
            &mut conn,
            "aaa",
            "session.created",
            &serde_json::json!({"sessionID": "aaa"}),
            1,
            None,
        )
        .unwrap();
        let b = append_durable(
            &mut conn,
            "bbb",
            "session.created",
            &serde_json::json!({"sessionID": "bbb"}),
            2,
            None,
        )
        .unwrap();
        assert_eq!((a.seq, b.seq), (0, 0));
        // Fork-style inheritance: reserve an inherited prefix, then the child starts at its own seq.
        reserve_sequence(&conn, "child", 41).unwrap();
        assert_eq!(latest_sequence(&conn, "child").unwrap(), 41);
        let inherited = append_durable(
            &mut conn,
            "child",
            "session.created",
            &serde_json::json!({"sessionID": "child"}),
            3,
            None,
        )
        .unwrap();
        assert_eq!(inherited.seq, 42);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn events_cascade_when_aggregate_sequence_row_is_deleted() {
        let (dir, database) = temp_db();
        let mut conn = database.conn;
        append_durable(
            &mut conn,
            "gone",
            "session.created",
            &serde_json::json!({"sessionID": "gone"}),
            1,
            None,
        )
        .unwrap();
        conn.execute("DELETE FROM event_sequence WHERE aggregate_id = 'gone'", [])
            .unwrap();
        let remaining: i64 = conn
            .query_row(
                "SELECT count(*) FROM event WHERE aggregate_id = 'gone'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(remaining, 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
