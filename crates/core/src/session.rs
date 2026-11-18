//! Session creation, listing and rename against SQLite, mirroring
//! packages/core/src/session/projector.ts (Created path) and store.ts.
//! The bun build inserts session_v2 rows from the durable `session.created`
//! event inside the projector; this port commits the event and the row in one
//! transaction (single process, no mid-flight crash window beyond SQLite's
//! own atomicity). Wire format, ids and table shape are identical.

use rusqlite::{params, Connection};

use crate::identifier;

const ADJECTIVES: &[&str] = &[
    "brave", "calm", "clever", "cosmic", "crisp", "curious", "eager", "gentle", "glowing", "happy",
    "hidden", "jolly", "kind", "lucky", "mighty", "misty", "neon", "nimble", "playful", "proud",
    "quick", "quiet", "shiny", "silent", "stellar", "sunny", "swift", "tidy", "witty",
];

const NOUNS: &[&str] = &[
    "cabin", "cactus", "canyon", "circuit", "comet", "eagle", "engine", "falcon", "forest",
    "garden", "harbor", "island", "knight", "lagoon", "meadow", "moon", "mountain", "nebula",
    "orchid", "otter", "panda", "pixel", "planet", "river", "rocket", "sailor", "squid", "star",
    "tiger", "wizard", "wolf",
];

pub fn slug() -> String {
    let mut bytes = [0u8; 2];
    getrandom::getrandom(&mut bytes).expect("system RNG unavailable");
    format!(
        "{}-{}",
        ADJECTIVES[bytes[0] as usize % ADJECTIVES.len()],
        NOUNS[bytes[1] as usize % NOUNS.len()]
    )
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionRecord {
    pub id: String,
    pub project_id: String,
    pub slug: String,
    pub directory: String,
    pub title: Option<String>,
    pub agent: Option<String>,
    pub version: String,
    pub cost: f64,
    pub tokens_input: i64,
    pub tokens_output: i64,
    pub time_created: i64,
    pub time_updated: i64,
}

pub struct CreateSession {
    pub project_id: String,
    pub directory: String,
    pub workspace_id: Option<String>,
    pub parent_id: Option<String>,
    pub title: Option<String>,
    pub agent: Option<String>,
    pub model: Option<serde_json::Value>,
    pub metadata: Option<serde_json::Value>,
    pub permissions: Option<serde_json::Value>,
    pub version: String,
    pub created: i64,
}

/// Build the `session.created` durable event payload, same shape as
/// packages/schema/src/session-event.ts `Created`.
fn created_payload(input: &CreateSession, session_id: &str, slug: &str) -> serde_json::Value {
    let mut location = serde_json::json!({ "directory": input.directory });
    if let Some(workspace_id) = &input.workspace_id {
        location["workspaceID"] = serde_json::json!(workspace_id);
    }
    let mut payload = serde_json::json!({
        "sessionID": session_id,
        "projectID": input.project_id,
        "location": location,
        "slug": slug,
        "version": input.version,
    });
    if let Some(parent_id) = &input.parent_id {
        payload["parentID"] = serde_json::json!(parent_id);
    }
    if let Some(title) = &input.title {
        payload["title"] = serde_json::json!(title);
    }
    if let Some(agent) = &input.agent {
        payload["agent"] = serde_json::json!(agent);
    }
    if let Some(model) = &input.model {
        payload["model"] = model.clone();
    }
    if let Some(metadata) = &input.metadata {
        payload["metadata"] = metadata.clone();
    }
    if let Some(permissions) = &input.permissions {
        payload["permissions"] = permissions.clone();
    }
    payload
}

#[cfg(test)]
fn insert_project(
    conn: &Connection,
    project_id: &str,
    worktree: &str,
    now: i64,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO project (id, worktree, vcs, name, time_created, time_updated, time_active, sandboxes) \
         VALUES (?1, ?2, NULL, NULL, ?3, ?3, ?3, '[]')",
        params![project_id, worktree, now],
    )?;
    Ok(())
}

/// Create a session: durable `session.created` event + `session_v2` row, one transaction.
pub fn create(conn: &mut Connection, input: CreateSession) -> rusqlite::Result<SessionRecord> {
    let session_id = format!("ses_{}", identifier::descending());
    let slug = slug();
    let payload = created_payload(&input, &session_id, &slug);
    let tx = conn.transaction()?;
    let seq = crate::latest_sequence(&tx, &session_id)? + 1;
    crate::reserve_sequence(&tx, &session_id, seq)?;
    let event_id = format!("evt_{}", identifier::ascending());
    tx.execute(
        "INSERT INTO event (id, aggregate_id, seq, created, type, data) VALUES (?1, ?2, ?3, ?4, 'session.created', ?5)",
        params![event_id, session_id, seq, input.created, payload.to_string()],
    )?;
    let model = input.model.as_ref().map(serde_json::Value::to_string);
    let metadata = input.metadata.as_ref().map(serde_json::Value::to_string);
    let permission = input.permissions.as_ref().map(serde_json::Value::to_string);
    tx.execute(
        "INSERT INTO session_v2 (id, project_id, workspace_id, parent_id, slug, directory, title, version, agent, model, metadata, permission, cost, tokens_input, tokens_output, tokens_reasoning, tokens_cache_read, tokens_cache_write, time_created, time_updated) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 0, 0, 0, 0, 0, 0, ?13, ?13)",
        params![session_id, input.project_id, input.workspace_id, input.parent_id, slug, input.directory, input.title, input.version, input.agent, model, metadata, permission, input.created],
    )?;
    tx.commit()?;
    Ok(SessionRecord {
        id: session_id,
        project_id: input.project_id,
        slug,
        directory: input.directory,
        title: input.title,
        agent: input.agent,
        version: input.version,
        cost: 0.0,
        tokens_input: 0,
        tokens_output: 0,
        time_created: input.created,
        time_updated: input.created,
    })
}

fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionRecord> {
    Ok(SessionRecord {
        id: row.get(0)?,
        project_id: row.get(1)?,
        slug: row.get(2)?,
        directory: row.get(3)?,
        title: row.get(4)?,
        agent: row.get(5)?,
        version: row.get(6)?,
        cost: row.get(7)?,
        tokens_input: row.get(8)?,
        tokens_output: row.get(9)?,
        time_created: row.get(10)?,
        time_updated: row.get(11)?,
    })
}

const SELECT: &str =
    "SELECT id, project_id, slug, directory, title, agent, version, cost, tokens_input, tokens_output, time_created, time_updated FROM session_v2";

pub fn get(conn: &Connection, session_id: &str) -> rusqlite::Result<Option<SessionRecord>> {
    let mut statement = conn.prepare(&format!("{SELECT} WHERE id = ?1"))?;
    let mut rows = statement.query_map(params![session_id], from_row)?;
    match rows.next() {
        Some(row) => Ok(Some(row?)),
        None => Ok(None),
    }
}

pub struct ListFilter {
    pub directory: Option<String>,
    pub project_id: Option<String>,
    pub limit: Option<i64>,
    pub order: &'static str,
}

pub fn list(conn: &Connection, filter: &ListFilter) -> rusqlite::Result<Vec<SessionRecord>> {
    let mut sql = String::from(SELECT);
    let mut parameters: Vec<String> = Vec::new();
    let mut where_clauses = Vec::new();
    if let Some(directory) = &filter.directory {
        where_clauses.push(format!("directory = ?{}", parameters.len() + 1));
        parameters.push(directory.clone());
    }
    if let Some(project_id) = &filter.project_id {
        where_clauses.push(format!("project_id = ?{}", parameters.len() + 1));
        parameters.push(project_id.clone());
    }
    if !where_clauses.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&where_clauses.join(" AND "));
    }
    sql.push_str(match filter.order {
        "asc" => " ORDER BY time_created ASC",
        _ => " ORDER BY time_created DESC",
    });
    if let Some(limit) = filter.limit {
        sql.push_str(&format!(" LIMIT {limit}"));
    }
    let mut statement = conn.prepare(&sql)?;
    let bound: Vec<&dyn rusqlite::ToSql> = parameters
        .iter()
        .map(|value| value as &dyn rusqlite::ToSql)
        .collect();
    let rows = statement
        .query_map(bound.as_slice(), from_row)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Resolve the project for `directory` (persisting it) and create a session there.
/// Sessions require a known project row; unknown project ids are rejected by the FK.
pub fn create_for_directory(
    conn: &mut Connection,
    directory: &str,
    now: i64,
    title: Option<String>,
    version: &str,
) -> rusqlite::Result<SessionRecord> {
    let resolved = crate::project::resolve(std::path::Path::new(directory));
    crate::project::persist(conn, &resolved, now)?;
    create(
        conn,
        CreateSession {
            project_id: resolved.id,
            directory: resolved.directory,
            workspace_id: None,
            parent_id: None,
            title,
            agent: None,
            model: None,
            metadata: None,
            permissions: None,
            version: version.to_owned(),
            created: now,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use std::path::PathBuf;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("devcode-session-{tag}-{}", identifier::ascending()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn temp_db(tag: &str) -> (PathBuf, Database) {
        let dir = temp_dir(tag);
        let database = Database::open(&dir.join("devcode.db")).unwrap();
        (dir, database)
    }

    #[test]
    fn created_session_has_event_row_and_session_row() {
        let (dir, database) = temp_db("create");
        let mut conn = database.conn;
        insert_project(&conn, "global", &dir.to_string_lossy(), 1).unwrap();
        let session = create(
            &mut conn,
            CreateSession {
                project_id: "global".into(),
                directory: dir.to_string_lossy().into_owned(),
                workspace_id: None,
                parent_id: None,
                title: Some("test session".into()),
                agent: Some("build".into()),
                model: Some(serde_json::json!({"id": "claude", "providerID": "anthropic", "variant": "default"})),
                metadata: None,
                permissions: None,
                version: "0.1.0-rust".into(),
                created: 1234,
            },
        )
        .unwrap();
        assert!(session.id.starts_with("ses_"));
        assert_eq!(session.slug.split('-').count(), 2);

        let events = crate::read_log(&conn, &session.id, -1, 10).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].r#type, "session.created");
        assert_eq!(events[0].data["slug"], session.slug);
        assert_eq!(events[0].data["projectID"], "global");

        let fetched = get(&conn, &session.id).unwrap().expect("session row");
        assert_eq!(fetched.title.as_deref(), Some("test session"));
        assert_eq!(fetched.agent.as_deref(), Some("build"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn create_for_directory_uses_resolved_project() {
        let (dir, database) = temp_db("resolve");
        let mut conn = database.conn;
        let session = create_for_directory(
            &mut conn,
            &dir.to_string_lossy(),
            7,
            Some("t".into()),
            "0.1.0-rust",
        )
        .unwrap();
        let project: String = conn
            .query_row(
                "SELECT id FROM project WHERE worktree = ?1",
                params![session.directory],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(session.project_id, project);
        assert_ne!(session.project_id, "global");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn list_filters_orders_and_limits() {
        let (dir, database) = temp_db("list");
        let mut conn = database.conn;
        for (id, created) in [("project_a", 1000), ("project_b", 2000)] {
            insert_project(&conn, id, &dir.to_string_lossy(), created).unwrap();
        }
        for (project, created) in [
            ("project_a", 1000),
            ("project_b", 2000),
            ("project_a", 3000),
        ] {
            create(
                &mut conn,
                CreateSession {
                    project_id: project.into(),
                    directory: dir.to_string_lossy().into_owned(),
                    workspace_id: None,
                    parent_id: None,
                    title: None,
                    agent: None,
                    model: None,
                    metadata: None,
                    permissions: None,
                    version: "0.1.0-rust".into(),
                    created,
                },
            )
            .unwrap();
        }
        let rows = list(
            &conn,
            &ListFilter {
                directory: Some(dir.to_string_lossy().into_owned()),
                project_id: Some("project_a".into()),
                limit: None,
                order: "desc",
            },
        )
        .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].time_created, 3000);
        let limited = list(
            &conn,
            &ListFilter {
                directory: None,
                project_id: None,
                limit: Some(2),
                order: "asc",
            },
        )
        .unwrap();
        assert_eq!(limited.len(), 2);
        assert_eq!(limited[0].time_created, 1000);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn unknown_project_is_rejected_by_foreign_keys() {
        let (dir, database) = temp_db("fk");
        let mut conn = database.conn;
        let result = create(
            &mut conn,
            CreateSession {
                project_id: "does-not-exist".into(),
                directory: dir.to_string_lossy().into_owned(),
                workspace_id: None,
                parent_id: None,
                title: None,
                agent: None,
                model: None,
                metadata: None,
                permissions: None,
                version: "0.1.0-rust".into(),
                created: 1,
            },
        );
        assert!(result.is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
