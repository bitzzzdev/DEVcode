//! Reads and writes for the HTTP session surface. Mirrors the wire shapes the
//! Bun build serves: PublicSessionInfo (protocol groups/session.ts), the
//! sessions list anchor pagination (session/store.ts) and the message list
//! (session/store.ts messages + history.ts withParts/decodeMessageRow).
//! Durable events are committed in the same transaction as the row update,
//! the same single-process deviation the rest of this crate documents.

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub id: String,
    pub project_id: String,
    pub workspace_id: Option<String>,
    pub parent_id: Option<String>,
    pub fork_session_id: Option<String>,
    pub fork_boundary: Option<Value>,
    pub directory: String,
    pub path: Option<String>,
    pub title: Option<String>,
    pub agent: Option<String>,
    pub model: Option<Value>,
    pub cost: f64,
    pub tokens_input: i64,
    pub tokens_output: i64,
    pub tokens_reasoning: i64,
    pub tokens_cache_read: i64,
    pub tokens_cache_write: i64,
    pub revert: Option<Value>,
    pub permissions: Option<Value>,
    pub metadata: Option<Value>,
    pub time_created: i64,
    pub time_updated: i64,
    pub time_busy: Option<i64>,
    pub time_idle: Option<i64>,
    pub time_viewed: Option<i64>,
    pub time_archived: Option<i64>,
    pub idle_outcome: Option<String>,
}

const FULL: &str = "SELECT id, project_id, workspace_id, parent_id, fork_session_id, fork_boundary,
    directory, path, title, agent, model, cost,
    tokens_input, tokens_output, tokens_reasoning, tokens_cache_read, tokens_cache_write,
    revert, permission, metadata,
    time_created, time_updated, time_busy, time_idle, time_viewed, time_archived, idle_outcome
FROM session_v2";

fn json_column(value: Option<String>) -> rusqlite::Result<Option<Value>> {
    value
        .map(|text| serde_json::from_str(&text))
        .transpose()
        .map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })
}

fn info_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionInfo> {
    Ok(SessionInfo {
        id: row.get(0)?,
        project_id: row.get(1)?,
        workspace_id: row.get(2)?,
        parent_id: row.get(3)?,
        fork_session_id: row.get(4)?,
        fork_boundary: json_column(row.get(5)?)?,
        directory: row.get(6)?,
        path: row.get(7)?,
        title: row.get(8)?,
        agent: row.get(9)?,
        model: json_column(row.get(10)?)?,
        cost: row.get(11)?,
        tokens_input: row.get(12)?,
        tokens_output: row.get(13)?,
        tokens_reasoning: row.get(14)?,
        tokens_cache_read: row.get(15)?,
        tokens_cache_write: row.get(16)?,
        revert: json_column(row.get(17)?)?,
        permissions: json_column(row.get(18)?)?,
        metadata: json_column(row.get(19)?)?,
        time_created: row.get(20)?,
        time_updated: row.get(21)?,
        time_busy: row.get(22)?,
        time_idle: row.get(23)?,
        time_viewed: row.get(24)?,
        time_archived: row.get(25)?,
        idle_outcome: row.get(26)?,
    })
}

pub fn get_info(conn: &Connection, session_id: &str) -> rusqlite::Result<Option<SessionInfo>> {
    let mut statement = conn.prepare(&format!("{FULL} WHERE id = ?1"))?;
    let mut rows = statement.query_map(params![session_id], info_from_row)?;
    match rows.next() {
        Some(row) => Ok(Some(row?)),
        None => Ok(None),
    }
}

/// Wire shape of PublicSessionInfo (protocol/groups/session.ts). `location` drops
/// workspaceID, everything else passes through with the same key names.
pub fn public_json(info: &SessionInfo) -> Value {
    let mut time = json!({ "created": info.time_created, "updated": info.time_updated });
    if let Some(busy) = info.time_busy {
        time["busy"] = json!(busy);
    }
    if let Some(idle) = info.time_idle {
        time["idle"] = json!(idle);
    }
    if let Some(viewed) = info.time_viewed {
        time["viewed"] = json!(viewed);
    }
    if let Some(archived) = info.time_archived {
        time["archived"] = json!(archived);
    }
    let mut out = json!({
        "id": info.id,
        "projectID": info.project_id,
        "cost": info.cost,
        "tokens": {
            "input": info.tokens_input,
            "output": info.tokens_output,
            "reasoning": info.tokens_reasoning,
            "cache": { "read": info.tokens_cache_read, "write": info.tokens_cache_write },
        },
        "time": time,
        "location": { "directory": info.directory },
    });
    if let Some(parent_id) = &info.parent_id {
        out["parentID"] = json!(parent_id);
    }
    if let Some(fork_session_id) = &info.fork_session_id {
        let boundary = info.fork_boundary.clone().unwrap_or(Value::Null);
        out["fork"] = json!({ "sessionID": fork_session_id, "boundary": boundary });
    }
    if let Some(agent) = &info.agent {
        out["agent"] = json!(agent);
    }
    if let Some(model) = &info.model {
        out["model"] = model.clone();
    }
    if let Some(outcome) = &info.idle_outcome {
        out["outcome"] = json!(outcome);
    }
    if let Some(title) = &info.title {
        out["title"] = json!(title);
    }
    if let Some(path) = &info.path {
        out["subpath"] = json!(path);
    }
    if let Some(metadata) = &info.metadata {
        out["metadata"] = metadata.clone();
    }
    if let Some(permissions) = &info.permissions {
        out["permissions"] = permissions.clone();
    }
    if let Some(revert) = &info.revert {
        out["revert"] = revert.clone();
    }
    out
}

#[derive(Default)]
pub struct InfoFilter {
    pub directory: Option<String>,
    pub project: Option<String>,
    pub subpath: Option<String>,
    pub search: Option<String>,
    /// None = absent, Some(None) = root sessions only, Some(Some(id)) = children of id.
    pub parent_id: Option<Option<String>>,
    pub archived: Option<bool>,
    /// "asc" | "desc"; defaults to desc.
    pub order: Option<String>,
    pub anchor: Option<Anchor>,
    pub limit: i64,
}

pub struct Anchor {
    pub id: String,
    pub time: i64,
    /// "next" | "previous"
    pub direction: String,
}

/// Session titles are matched with a GLOB case-class pattern; `%`/`_` keep their
/// LIKE meaning, other metacharacters stay literal. Same transformation as
/// session/store.ts titlePattern, with the same single-char case folding.
fn glob_pattern(search: &str) -> String {
    let mut pattern = String::from("*");
    for char in search.chars() {
        match char {
            '%' => pattern.push('*'),
            '_' => pattern.push('?'),
            '*' | '?' | '[' => {
                pattern.push('[');
                pattern.push(char);
                pattern.push(']');
            }
            _ => {
                let lower: Vec<char> = char.to_lowercase().collect();
                let upper: Vec<char> = char.to_uppercase().collect();
                let mut forms = vec![char];
                for form in [lower.as_slice(), upper.as_slice()] {
                    if form.len() == 1 && !forms.contains(&form[0]) {
                        forms.push(form[0]);
                    }
                }
                if forms.len() > 1 {
                    pattern.push('[');
                    pattern.extend(forms);
                    pattern.push(']');
                    continue;
                }
                pattern.push(char);
            }
        }
    }
    pattern.push('*');
    pattern
}

/// One page of sessions, ordered by time_updated like the Bun store. The anchor
/// direction flips the effective order and reverses the page, matching store.ts.
pub fn list_info(conn: &Connection, filter: &InfoFilter) -> rusqlite::Result<Vec<SessionInfo>> {
    let requested = filter.order.as_deref().unwrap_or("desc");
    let direction = filter
        .anchor
        .as_ref()
        .map(|anchor| anchor.direction.as_str())
        .unwrap_or("next");
    let order = if direction == "previous" {
        if requested == "asc" {
            "desc"
        } else {
            "asc"
        }
    } else {
        requested
    };
    let mut conditions: Vec<String> = Vec::new();
    let mut parameters: Vec<rusqlite::types::Value> = Vec::new();
    let push = |conditions: &mut Vec<String>,
                parameters: &mut Vec<rusqlite::types::Value>,
                condition: String,
                values: Vec<rusqlite::types::Value>| {
        let mut values = values.into_iter();
        let mut built = String::with_capacity(condition.len() + 16);
        for ch in condition.chars() {
            if ch == '?' {
                parameters.push(values.next().expect("placeholder count mismatch"));
                built.push_str(&format!("?{}", parameters.len()));
                continue;
            }
            built.push(ch);
        }
        assert!(values.next().is_none(), "placeholder count mismatch");
        conditions.push(built);
    };
    let text = |value: &String| rusqlite::types::Value::Text(value.clone());
    if let Some(directory) = &filter.directory {
        push(
            &mut conditions,
            &mut parameters,
            "directory = ?".into(),
            vec![text(directory)],
        );
    }
    if let Some(project) = &filter.project {
        push(
            &mut conditions,
            &mut parameters,
            "project_id = ?".into(),
            vec![text(project)],
        );
    }
    if let Some(subpath) = &filter.subpath {
        push(
            &mut conditions,
            &mut parameters,
            "path = ?".into(),
            vec![text(subpath)],
        );
    }
    if let Some(search) = &filter.search {
        push(
            &mut conditions,
            &mut parameters,
            "title GLOB ?".into(),
            vec![rusqlite::types::Value::Text(glob_pattern(search))],
        );
    }
    match &filter.parent_id {
        Some(None) => conditions.push("parent_id IS NULL".to_owned()),
        Some(Some(parent_id)) => {
            push(
                &mut conditions,
                &mut parameters,
                "parent_id = ?".into(),
                vec![text(parent_id)],
            );
        }
        None => {}
    }
    match filter.archived {
        Some(true) => conditions.push("time_archived IS NOT NULL".to_owned()),
        Some(false) => conditions.push("time_archived IS NULL".to_owned()),
        None => {}
    }
    if let Some(anchor) = &filter.anchor {
        let cmp = if order == "asc" { ">" } else { "<" };
        push(
            &mut conditions,
            &mut parameters,
            format!("(time_updated {cmp} ? OR (time_updated = ? AND id {cmp} ?))"),
            vec![
                rusqlite::types::Value::Integer(anchor.time),
                rusqlite::types::Value::Integer(anchor.time),
                rusqlite::types::Value::Text(anchor.id.clone()),
            ],
        );
    }
    let mut sql = String::from(FULL);
    if !conditions.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&conditions.join(" AND "));
    }
    let direction_sql = if order == "asc" { "ASC" } else { "DESC" };
    sql.push_str(&format!(
        " ORDER BY time_updated {direction_sql}, id {direction_sql}"
    ));
    sql.push_str(&format!(" LIMIT {}", filter.limit));
    let mut statement = conn.prepare(&sql)?;
    let bound: Vec<&dyn rusqlite::ToSql> = parameters
        .iter()
        .map(|value| value as &dyn rusqlite::ToSql)
        .collect();
    let rows = statement
        .query_map(bound.as_slice(), info_from_row)?
        .collect::<Result<Vec<_>, _>>()?;
    if direction == "previous" {
        return Ok(rows.into_iter().rev().collect());
    }
    Ok(rows)
}

fn session_exists(conn: &Connection, session_id: &str) -> rusqlite::Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM session_v2 WHERE id = ?1",
            params![session_id],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

fn append_event(
    conn: &Connection,
    event_type: &str,
    session_id: &str,
    payload: Value,
    now: i64,
) -> rusqlite::Result<i64> {
    let seq = crate::latest_sequence(conn, session_id)? + 1;
    crate::reserve_sequence(conn, session_id, seq)?;
    let event_id = format!("evt_{}", crate::ascending());
    conn.execute(
        "INSERT INTO event (id, aggregate_id, seq, created, type, data) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![event_id, session_id, seq, now, event_type, payload.to_string()],
    )?;
    Ok(seq)
}

/// Rename with a non-empty title (`session.renamed`). Empty titles are an error at the
/// boundary; auto-title generation is not ported.
pub fn rename(
    conn: &mut Connection,
    session_id: &str,
    title: &str,
    now: i64,
) -> rusqlite::Result<bool> {
    let tx = conn.transaction()?;
    if !session_exists(&tx, session_id)? {
        return Ok(false);
    }
    append_event(
        &tx,
        "session.renamed",
        session_id,
        json!({ "sessionID": session_id, "title": title }),
        now,
    )?;
    tx.execute(
        "UPDATE session_v2 SET title = ?2, time_updated = ?3 WHERE id = ?1",
        params![session_id, title, now],
    )?;
    tx.commit()?;
    Ok(true)
}

pub fn set_metadata(
    conn: &mut Connection,
    session_id: &str,
    metadata: &Value,
    now: i64,
) -> rusqlite::Result<bool> {
    let tx = conn.transaction()?;
    if !session_exists(&tx, session_id)? {
        return Ok(false);
    }
    append_event(
        &tx,
        "session.metadata.updated",
        session_id,
        json!({ "sessionID": session_id, "metadata": metadata }),
        now,
    )?;
    tx.execute(
        "UPDATE session_v2 SET metadata = ?2, time_updated = ?3 WHERE id = ?1",
        params![session_id, metadata.to_string(), now],
    )?;
    tx.commit()?;
    Ok(true)
}

pub fn set_permissions(
    conn: &mut Connection,
    session_id: &str,
    permissions: &Value,
    now: i64,
) -> rusqlite::Result<bool> {
    let tx = conn.transaction()?;
    if !session_exists(&tx, session_id)? {
        return Ok(false);
    }
    append_event(
        &tx,
        "session.permissions",
        session_id,
        json!({ "sessionID": session_id, "permissions": permissions }),
        now,
    )?;
    tx.execute(
        "UPDATE session_v2 SET permission = ?2, time_updated = ?3 WHERE id = ?1",
        params![session_id, permissions.to_string(), now],
    )?;
    tx.commit()?;
    Ok(true)
}

pub fn set_archived(
    conn: &mut Connection,
    session_id: &str,
    archived: bool,
    now: i64,
) -> rusqlite::Result<bool> {
    let tx = conn.transaction()?;
    if !session_exists(&tx, session_id)? {
        return Ok(false);
    }
    let event_type = if archived {
        "session.archived"
    } else {
        "session.unarchived"
    };
    append_event(
        &tx,
        event_type,
        session_id,
        json!({ "sessionID": session_id }),
        now,
    )?;
    if archived {
        tx.execute(
            "UPDATE session_v2 SET time_archived = ?2, time_updated = ?2 WHERE id = ?1",
            params![session_id, now],
        )?;
    } else {
        tx.execute(
            "UPDATE session_v2 SET time_archived = NULL, time_updated = ?2 WHERE id = ?1",
            params![session_id, now],
        )?;
    }
    tx.commit()?;
    Ok(true)
}

/// Delete a session and its child sessions. Writes the durable `session.deleted`
/// event, removes the session_v2 row (FK cascades messages/parts/inbox/blob
/// refs), then releases blobs nothing references anymore. The parent row is
/// deleted before its children, matching the Bun ordering. Durable events are
/// retained; they are the log.
pub fn delete_session(conn: &mut Connection, session_id: &str, now: i64) -> rusqlite::Result<bool> {
    let tx = conn.transaction()?;
    if !session_exists(&tx, session_id)? {
        return Ok(false);
    }
    let children: Vec<String> = {
        let mut statement =
            tx.prepare("SELECT id FROM session_v2 WHERE parent_id = ?1 ORDER BY rowid")?;
        let collected = statement
            .query_map(params![session_id], |row| row.get(0))?
            .collect::<Result<Vec<String>, _>>()?;
        collected
    };
    let hashes: Vec<String> = {
        let mut statement =
            tx.prepare("SELECT DISTINCT hash FROM blob_ref WHERE session_id = ?1")?;
        let collected = statement
            .query_map(params![session_id], |row| row.get(0))?
            .collect::<Result<Vec<String>, _>>()?;
        collected
    };
    append_event(
        &tx,
        "session.deleted",
        session_id,
        json!({ "sessionID": session_id }),
        now,
    )?;
    tx.execute("DELETE FROM session_v2 WHERE id = ?1", params![session_id])?;
    if !hashes.is_empty() {
        let placeholders = hashes.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        tx.execute(
            &format!(
                "DELETE FROM blob WHERE hash IN ({placeholders}) AND NOT EXISTS (SELECT 1 FROM blob_ref WHERE hash = blob.hash)"
            ),
            rusqlite::params_from_iter(hashes.iter()),
        )?;
    }
    tx.commit()?;
    for child in children {
        delete_session(conn, &child, now)?;
    }
    Ok(true)
}

pub struct CreateViaApi {
    pub id: Option<String>,
    pub parent_id: Option<String>,
    pub directory: Option<String>,
    pub workspace_id: Option<String>,
    pub title: Option<String>,
    pub agent: Option<String>,
    pub model: Option<Value>,
    pub metadata: Option<Value>,
    pub permissions: Option<Value>,
    pub version: String,
    pub now: i64,
}

pub enum CreateError {
    ParentNotFound(String),
    Conflict(String),
    Database(rusqlite::Error),
}

/// Create a session from the HTTP endpoint. With a parent, the child inherits the
/// parent's project, directory and workspace; otherwise the location directory (or
/// the service's cwd) is resolved to a project. Mirrors handlers/session.ts
/// session.create.
pub fn create_via_api(
    conn: &mut Connection,
    input: CreateViaApi,
) -> Result<SessionInfo, CreateError> {
    if let Some(id) = &input.id {
        if get_info(conn, id).map_err(CreateError::Database)?.is_some() {
            return Err(CreateError::Conflict(id.clone()));
        }
    }
    let (project_id, directory, workspace_id) = match &input.parent_id {
        Some(parent_id) => {
            let parent = get_info(conn, parent_id)
                .map_err(CreateError::Database)?
                .ok_or_else(|| CreateError::ParentNotFound(parent_id.clone()))?;
            (parent.project_id, parent.directory, parent.workspace_id)
        }
        None => {
            let directory = input.directory.clone().unwrap_or_else(|| {
                std::env::current_dir()
                    .map(|path| path.to_string_lossy().into_owned())
                    .unwrap_or_default()
            });
            let resolved = crate::resolve_project(std::path::Path::new(&directory));
            crate::persist_project(conn, &resolved, input.now).map_err(CreateError::Database)?;
            (resolved.id, resolved.directory, input.workspace_id.clone())
        }
    };
    let record = crate::create_session(
        conn,
        crate::CreateSession {
            id: input.id,
            project_id,
            directory,
            workspace_id,
            parent_id: input.parent_id,
            title: input.title,
            agent: input.agent,
            model: input.model,
            metadata: input.metadata,
            permissions: input.permissions,
            version: input.version,
            created: input.now,
        },
    )
    .map_err(CreateError::Database)?;
    get_info(conn, &record.id)
        .map_err(CreateError::Database)?
        .ok_or_else(|| CreateError::Database(rusqlite::Error::QueryReturnedNoRows))
}
