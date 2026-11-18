//! `session_message` / `session_part` / `session_inbox` persistence for the
//! runner. Mirrors the writes in packages/core/src/session/projector.ts:
//! assistant rows keep an empty `content: []` envelope and their parts live in
//! `session_part`; user rows are inserted when the inbox item is delivered.
//!
//! Events are appended via `event::append_durable` and these projections are
//! written afterwards, in a separate transaction — the same "event first,
//! projector second" split the Bun build has. A crash in between loses the
//! projection, not the event.

use rusqlite::{params, Connection};
use serde_json::{json, Value};

/// Insert the `session_inbox` row for a durable `session.inbox.enqueued` event.
pub fn project_admitted(
    conn: &Connection,
    id: &str,
    session_id: &str,
    item: &Value,
    enqueued_seq: i64,
    time_created: i64,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO session_inbox (id, session_id, type, payload, delivery, enqueued_seq, time_created)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            id,
            session_id,
            item["type"].as_str().unwrap_or("user"),
            item["payload"].to_string(),
            item["delivery"].as_str().unwrap_or("steer"),
            enqueued_seq,
            time_created,
        ],
    )?;
    Ok(())
}

/// Delete the inbox row for `session.inbox.delivered`; returns the stored item.
/// The caller inserts the message row with the delivered event's durable seq.
pub fn project_delivered(
    conn: &Connection,
    id: &str,
    session_id: &str,
) -> rusqlite::Result<Option<(String, Value)>> {
    let row = conn
        .query_row(
            "DELETE FROM session_inbox WHERE id = ?1 AND session_id = ?2 RETURNING type, payload",
            params![id, session_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    Ok(row.map(|(row_type, payload)| {
        (
            row_type,
            serde_json::from_str(&payload).unwrap_or_else(|_| json!({})),
        )
    }))
}

use rusqlite::OptionalExtension;

/// Insert a user/synthetic message row; `seq` is the delivered event's durable seq.
pub fn insert_message(
    conn: &Connection,
    id: &str,
    session_id: &str,
    message_type: &str,
    seq: i64,
    created: i64,
    data: Value,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO session_message (id, session_id, type, seq, time_created, time_updated, data) VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6)",
        params![id, session_id, message_type, seq, created, data.to_string()],
    )?;
    Ok(())
}

/// Assistant message row written by `session.step.started`.
pub fn insert_assistant(
    conn: &Connection,
    id: &str,
    session_id: &str,
    seq: i64,
    agent: &str,
    model: &Value,
    created: i64,
) -> rusqlite::Result<()> {
    let data =
        json!({ "agent": agent, "model": model, "time": { "created": created }, "content": [] });
    insert_message(conn, id, session_id, "assistant", seq, created, data)
}

/// Apply the terminal fields of `session.step.ended` / `session.step.failed` to the assistant row.
pub fn finish_assistant(
    conn: &Connection,
    id: &str,
    finish: Option<&str>,
    raw_finish: Option<&str>,
    cost: f64,
    tokens: &Value,
    completed: i64,
) -> rusqlite::Result<()> {
    let row: Option<String> = conn
        .query_row(
            "SELECT data FROM session_message WHERE id = ?1",
            params![id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(row) = row else { return Ok(()) };
    let mut data: Value = serde_json::from_str(&row).unwrap_or_else(|_| json!({}));
    data["content"] = json!([]);
    if let Some(finish) = finish {
        data["finish"] = Value::String(finish.to_owned());
    }
    if let Some(raw) = raw_finish {
        data["rawFinish"] = Value::String(raw.to_owned());
    }
    data["cost"] = json!(cost);
    data["tokens"] = tokens.clone();
    data["time"]["completed"] = json!(completed);
    conn.execute(
        "UPDATE session_message SET data = ?2, time_updated = ?3 WHERE id = ?1",
        params![id, data.to_string(), completed],
    )?;
    Ok(())
}

/// Replace an assistant message's parts (the `session.message.content.updated` projection).
pub fn replace_parts(
    conn: &mut Connection,
    message_id: &str,
    session_id: &str,
    parts: &[Value],
    time_created: i64,
    time_updated: i64,
) -> rusqlite::Result<()> {
    let tx = conn.transaction()?;
    tx.execute(
        "DELETE FROM session_part WHERE message_id = ?1",
        params![message_id],
    )?;
    for (ordinal, part) in parts.iter().enumerate() {
        tx.execute(
            "INSERT INTO session_part (message_id, session_id, ordinal, type, call_id, time_created, time_updated, data)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                message_id,
                session_id,
                ordinal as i64,
                part["type"].as_str().unwrap_or("text"),
                part.get("id").and_then(Value::as_str).filter(|_| part["type"] == "tool"),
                time_created,
                time_updated,
                part.to_string(),
            ],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Add cost/tokens to the session row (`session.step.ended` → usage projection).
pub fn apply_usage(
    conn: &Connection,
    session_id: &str,
    cost: f64,
    tokens: &Value,
    time_updated: i64,
) -> rusqlite::Result<()> {
    let input = tokens["input"].as_f64().unwrap_or(0.0);
    let output = tokens["output"].as_f64().unwrap_or(0.0);
    let reasoning = tokens["reasoning"].as_f64().unwrap_or(0.0);
    let cache_read = tokens["cache"]["read"].as_f64().unwrap_or(0.0);
    let cache_write = tokens["cache"]["write"].as_f64().unwrap_or(0.0);
    conn.execute(
        "UPDATE session_v2 SET
           cost = cost + ?2,
           tokens_input = tokens_input + ?3,
           tokens_output = tokens_output + ?4,
           tokens_reasoning = tokens_reasoning + ?5,
           tokens_cache_read = tokens_cache_read + ?6,
           tokens_cache_write = tokens_cache_write + ?7,
           time_updated = ?8
         WHERE id = ?1",
        params![
            session_id,
            cost,
            input as i64,
            output as i64,
            reasoning as i64,
            cache_read as i64,
            cache_write as i64,
            time_updated
        ],
    )?;
    Ok(())
}

/// Mark the session idle with an outcome (`session.execution.succeeded|failed` projection).
pub fn mark_idle(
    conn: &Connection,
    session_id: &str,
    outcome: &str,
    time: i64,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE session_v2 SET
           time_idle = max(?3, coalesce(time_idle + 1, ?3)),
           idle_outcome = ?2,
           time_updated = ?3
         WHERE id = ?1",
        params![session_id, outcome, time],
    )?;
    Ok(())
}

pub struct HistoryRow {
    pub id: String,
    pub row_type: String,
    pub seq: i64,
    pub data: Value,
    pub parts: Vec<Value>,
}

fn attach_parts(
    conn: &Connection,
    rows: Vec<(String, String, i64, String)>,
) -> rusqlite::Result<Vec<HistoryRow>> {
    let ids: Vec<&str> = rows
        .iter()
        .filter(|row| row.1 == "assistant")
        .map(|row| row.0.as_str())
        .collect();
    let mut parts_by_id: std::collections::HashMap<String, Vec<Value>> =
        std::collections::HashMap::new();
    for ids in ids.chunks(8) {
        if ids.is_empty() {
            continue;
        }
        let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let mut part_statement = conn.prepare(&format!(
            "SELECT message_id, ordinal, data FROM session_part WHERE message_id IN ({placeholders}) ORDER BY message_id, ordinal"
        ))?;
        let part_rows = part_statement
            .query_map(rusqlite::params_from_iter(ids.iter()), |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(2)?))
            })?
            .filter_map(|row| row.ok())
            .collect::<Vec<_>>();
        for (message_id, part) in part_rows {
            parts_by_id
                .entry(message_id)
                .or_default()
                .push(serde_json::from_str(&part).unwrap_or(Value::Null));
        }
    }
    Ok(rows
        .into_iter()
        .map(|(id, row_type, seq, data)| {
            let parts = parts_by_id.remove(&id).unwrap_or_default();
            HistoryRow {
                id,
                row_type,
                seq,
                data: serde_json::from_str(&data).unwrap_or(Value::Null),
                parts,
            }
        })
        .collect())
}

fn read_rows(
    conn: &Connection,
    session_id: &str,
    where_sql: &str,
    order: &str,
    limit: i64,
    parameters: Vec<Value>,
) -> rusqlite::Result<Vec<(String, String, i64, String)>> {
    let sql = format!(
        "SELECT id, type, seq, data FROM session_message WHERE session_id = ?1{where_sql} ORDER BY seq {order} LIMIT {limit}"
    );
    let mut statement = conn.prepare(&sql)?;
    let mut bound: Vec<rusqlite::types::Value> = Vec::with_capacity(parameters.len() + 1);
    bound.push(rusqlite::types::Value::Text(session_id.to_owned()));
    for value in parameters {
        bound.push(match value {
            Value::String(text) => rusqlite::types::Value::Text(text),
            Value::Number(number) => match number.as_i64() {
                Some(integer) => rusqlite::types::Value::Integer(integer),
                None => rusqlite::types::Value::Real(number.as_f64().unwrap_or(0.0)),
            },
            other => rusqlite::types::Value::Text(other.to_string()),
        });
    }
    let bound: Vec<&dyn rusqlite::ToSql> = bound
        .iter()
        .map(|value| value as &dyn rusqlite::ToSql)
        .collect();
    let rows = statement
        .query_map(bound.as_slice(), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Read the session transcript in order with assistant parts attached (`history.ts` withParts).
pub fn history(conn: &Connection, session_id: &str) -> rusqlite::Result<Vec<HistoryRow>> {
    let rows = read_rows(conn, session_id, "", "ASC", i64::MAX, Vec::new())?;
    attach_parts(conn, rows)
}

/// One page of session messages. `requested_order` is "asc"|"desc"; `cursor` is the
/// decoded `{order, direction}` pair from the base64url JSON the endpoint returns.
/// A cursor naming a message that no longer exists yields an empty page, the same
/// as the Bun store.
pub fn messages_page(
    conn: &Connection,
    session_id: &str,
    limit: i64,
    requested_order: &str,
    type_filter: Option<&str>,
    cursor: Option<(&str, &str)>,
) -> rusqlite::Result<Vec<HistoryRow>> {
    let direction = cursor.map(|(_, direction)| direction).unwrap_or("next");
    let order = if direction == "previous" {
        if requested_order == "asc" {
            "desc"
        } else {
            "asc"
        }
    } else {
        requested_order
    };
    let mut where_sql = String::new();
    let mut parameters: Vec<Value> = Vec::new();
    if let Some((cursor_id, _)) = cursor {
        let anchor: Option<i64> = conn
            .query_row(
                "SELECT seq FROM session_message WHERE session_id = ?1 AND id = ?2",
                params![session_id, cursor_id],
                |row| row.get(0),
            )
            .ok();
        let Some(anchor) = anchor else {
            return Ok(Vec::new());
        };
        let cmp = if order == "asc" { ">" } else { "<" };
        where_sql.push_str(&format!(" AND seq {cmp} ?"));
        parameters.push(json!(anchor));
    }
    if let Some(type_filter) = type_filter {
        where_sql.push_str(" AND type = ?");
        parameters.push(json!(type_filter));
    }
    let mut rows = read_rows(
        conn,
        session_id,
        &where_sql,
        if order == "asc" { "ASC" } else { "DESC" },
        limit,
        parameters,
    )?;
    if direction == "previous" {
        rows.reverse();
    }
    attach_parts(conn, rows)
}

/// Wire shape of one projected message: the stored row data plus id/type. Assistant
/// rows swap their empty content envelope for the parts (history.ts withParts);
/// inline content present in a row is kept after the parts, the same order the
/// projector uses.
pub fn wire(row: &HistoryRow) -> Value {
    let mut data = row.data.clone();
    if row.row_type == "assistant" {
        let inline = data.get("content").cloned().unwrap_or(Value::Null);
        let content = match inline {
            Value::Array(items) if !items.is_empty() => {
                let mut merged = row.parts.clone();
                merged.extend(items);
                Value::Array(merged)
            }
            _ => Value::Array(row.parts.clone()),
        };
        data["content"] = content;
    }
    data["id"] = json!(row.id);
    data["type"] = json!(row.row_type);
    data
}
