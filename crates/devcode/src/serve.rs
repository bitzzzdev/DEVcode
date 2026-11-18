//! Runs the HTTP service: /api/info and a minimal /openapi.json, behind the same
//! basic-auth contract the Bun build uses. Everything else is out of scope until the
//! rest of the product ports over.

use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use base64::Engine;
use devcode_service as svc;
use serde_json::json;
use tiny_http::{Header, Request, Response, Server, StatusCode};

/// Sessions with an agent loop running in this process. Prompting a busy session
/// returns 409 instead of interleaving two runs over one history (the Bun build
/// serializes on its execution coordinator).
static RUNNING: std::sync::OnceLock<Mutex<std::collections::HashSet<String>>> =
    std::sync::OnceLock::new();

/// Plugin listings in the Bun `Plugin.Info` wire shape, loaded once at service startup
/// from `<config>/plugins/*/plugin.json` (native Rust cdylib plugins).
static PLUGIN_LIST: std::sync::OnceLock<Vec<serde_json::Value>> = std::sync::OnceLock::new();

fn plugin_list() -> &'static [serde_json::Value] {
    PLUGIN_LIST.get().map(Vec::as_slice).unwrap_or(&[])
}

fn load_plugins(paths: &devcode_util::Paths) -> Vec<serde_json::Value> {
    let dir = paths.config.join("plugins");
    if !dir.is_dir() {
        return Vec::new();
    }
    let mut host = devcode_plugin::Host::empty();
    // Native code from the user's own plugins directory, same trust level as the
    // Bun build's JS plugins from the same location.
    let (_loaded, errors) = unsafe { host.load_dir(&dir) };
    for error in &errors {
        eprintln!("plugin failed to load: {error}");
    }
    let infos = host.list();
    infos
        .iter()
        .map(|plugin| {
            json!({
                "id": plugin.info.id,
                "source": { "type": "local", "path": plugin.manifest_dir.to_string_lossy() },
                "features": { "server": true },
                "state": { "status": "active" },
            })
        })
        .chain(errors.iter().map(|error| {
            json!({
                "id": serde_json::Value::Null,
                "source": { "type": "local", "path": dir.to_string_lossy() },
                "features": { "server": true },
                "state": { "status": "failed", "error": error },
            })
        }))
        .collect()
}

fn running_insert(session_id: &str) -> bool {
    let mut set = RUNNING
        .get_or_init(|| Mutex::new(std::collections::HashSet::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    set.insert(session_id.to_owned())
}

fn running_remove(session_id: &str) {
    let Ok(mut set) = RUNNING
        .get_or_init(|| Mutex::new(std::collections::HashSet::new()))
        .lock()
    else {
        return;
    };
    set.remove(session_id);
}

pub fn run(args: &[String]) -> ExitCode {
    let service_mode = args.iter().any(|arg| arg == "--service");
    let paths = devcode_util::paths("devcode");
    let config_path = svc::config_file(&paths, devcode_util::CHANNEL);
    let mut config = svc::Config::read(&config_path);
    let hostname = config
        .get("hostname")
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| svc::DEFAULT_HOSTNAME.to_owned());
    let port = config
        .get_port()
        .unwrap_or_else(|| svc::default_port(devcode_util::CHANNEL));

    // The Bun build persists a generated password; do the same so `service get password` stays stable.
    let password = match std::env::var_os("DEVCODE_PASSWORD")
        .or_else(|| std::env::var_os("DEVCODE_SERVER_PASSWORD"))
    {
        Some(env) => env.to_string_lossy().into_owned(),
        None => match config.get_password() {
            Some(existing) => existing,
            None => {
                let generated = svc::generate_password();
                config.set("password", json!(generated));
                if let Err(error) = config.write(&config_path) {
                    eprintln!("failed to persist generated service password: {error}");
                    return ExitCode::FAILURE;
                }
                generated
            }
        },
    };

    let url = svc::service_url(&hostname, port);
    let registration = svc::Registration {
        id: svc::generate_id(),
        version: devcode_util::VERSION.to_owned(),
        url: url.clone(),
        pid: std::process::id() as i64,
        password: password.clone(),
    };

    // The Bun build opens (and bootstraps) the SQLite database at startup; do the same so
    // the managed service owns its storage file from the first connection.
    let database_path = devcode_core::database_path(&paths);
    let mut database = match devcode_core::Database::open(&database_path) {
        Ok(database) => database,
        Err(error) => {
            eprintln!(
                "failed to open database {}: {error}",
                database_path.display()
            );
            return ExitCode::FAILURE;
        }
    };
    let _ = PLUGIN_LIST.set(load_plugins(&paths));

    let addr = format!("{hostname}:{port}");
    let server = match Server::http(&addr) {
        Ok(server) => server,
        Err(_) => {
            eprintln!(
                "Managed service port {port} on {hostname} is already in use by another process."
            );
            eprintln!("Configure another port with `devcode service set port <port>` and start the service again.");
            return ExitCode::FAILURE;
        }
    };

    let registration_path = paths.state.join("service.json");
    if service_mode {
        if let Err(error) = registration.write(&registration_path) {
            eprintln!(
                "failed to write service registration {}: {error}",
                registration_path.display()
            );
            return ExitCode::FAILURE;
        }
    }

    let pid = std::process::id();
    let hostname_owned =
        std::fs::read_to_string("/proc/sys/kernel/hostname").unwrap_or_else(|_| "localhost".into());
    let info = json!({
        "version": devcode_util::VERSION,
        "pid": pid,
        "hostname": hostname_owned.trim(),
        "urls": [url],
        "paths": { "tmp": paths.tmp.to_string_lossy() },
        "capabilities": { "persistentPty": true },
    });

    let registered = Arc::new(Mutex::new(Some(registration.clone())));
    let shutdown = registered.clone();
    let registration_path_for_signal = registration_path.clone();
    ctrlc::set_handler(move || {
        let registration = shutdown.lock().unwrap().take();
        if let Some(registration) = registration {
            registration.remove_if_owned(&registration_path_for_signal);
        }
        std::process::exit(0);
    })
    .expect("failed to install signal handler");

    println!("server listening on {url}");
    if !service_mode {
        println!("server password {password}");
    }

    for request in server.incoming_requests() {
        handle(request, &info, &password, &mut database.conn);
    }
    ExitCode::SUCCESS
}

fn handle(
    mut request: Request,
    info: &serde_json::Value,
    password: &str,
    conn: &mut rusqlite::Connection,
) {
    if !authorized(&request, password) {
        let body = json!({"_tag": "UnauthorizedError", "message": "Authentication required"});
        respond(request, 401, body);
        return;
    }
    let method = request.method().to_string();
    let url = request.url().to_owned();
    let (path, query) = match url.split_once('?') {
        Some((path, query)) => (path.to_string(), query.to_string()),
        None => (url.clone(), String::new()),
    };
    use std::io::Read as _;
    let mut body = Vec::new();
    let reader = request.as_reader();
    let _ = reader.take(4 * 1024 * 1024 + 1).read_to_end(&mut body);
    if body.len() > 4 * 1024 * 1024 {
        let body = json!({"_tag": "InvalidRequestError", "message": "Request body exceeds 4 MiB"});
        respond(request, 400, body);
        return;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0);
    let (status, payload) = route(&method, &path, &query, &body, conn, info, now);
    match payload {
        Some(payload) => respond(request, status, payload),
        None => {
            let _ = request.respond(Response::empty(StatusCode(status)));
        }
    }
}

const MESSAGE_TYPES: [&str; 11] = [
    "agent-switched",
    "model-switched",
    "location-switched",
    "user",
    "synthetic",
    "system",
    "skill",
    "shell",
    "assistant",
    "compaction",
    "idle",
];

fn error(status: u16, tag: &str, message: &str) -> (u16, Option<serde_json::Value>) {
    (status, Some(json!({"_tag": tag, "message": message})))
}

fn cursor_error() -> (u16, Option<serde_json::Value>) {
    error(400, "InvalidCursorError", "Invalid cursor")
}

fn not_found(session_id: &str) -> (u16, Option<serde_json::Value>) {
    (
        404,
        Some(json!({
            "_tag": "SessionNotFoundError",
            "sessionID": session_id,
            "message": format!("Session not found: {session_id}"),
        })),
    )
}

fn percent_decode(segment: &str) -> String {
    let hex = |byte: u8| -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            b'A'..=b'F' => Some(byte - b'A' + 10),
            _ => None,
        }
    };
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' => match (
                bytes.get(index + 1).copied().and_then(hex),
                bytes.get(index + 2).copied().and_then(hex),
            ) {
                (Some(high), Some(low)) => {
                    out.push(high * 16 + low);
                    index += 3;
                }
                _ => {
                    out.push(b'%');
                    index += 1;
                }
            },
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_query(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| match pair.split_once('=') {
            Some((key, value)) => (percent_decode(key), percent_decode(value)),
            None => (percent_decode(pair), String::new()),
        })
        .collect()
}

fn qget<'a>(params: &'a [(String, String)], key: &str) -> Option<&'a str> {
    params
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.as_str())
}

fn query_limit(
    params: &[(String, String)],
    default: i64,
    max: i64,
) -> Result<i64, (u16, Option<serde_json::Value>)> {
    match qget(params, "limit") {
        Some(raw) => match raw.parse::<i64>() {
            Ok(limit) if limit >= 1 && limit <= max => Ok(limit),
            _ => Err(error(400, "InvalidRequestError", "Invalid limit")),
        },
        None => Ok(default),
    }
}

fn query_order(
    params: &[(String, String)],
) -> Result<Option<String>, (u16, Option<serde_json::Value>)> {
    match qget(params, "order") {
        Some(order @ ("asc" | "desc")) => Ok(Some(order.to_owned())),
        Some(_) => Err(error(400, "InvalidRequestError", "Invalid order")),
        None => Ok(None),
    }
}

fn b64url_encode(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn b64url_decode(input: &str) -> Result<Vec<u8>, ()> {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(input.trim_end_matches('='))
        .map_err(|_| ())
}

/// PublicSessionInfo list cursor: base64url JSON `{<filter echo>, anchor: {id, time, direction}}`,
/// the same shape ListCursor.make produces in the Bun build.
fn encode_session_cursor(
    filter: &devcode_core::session_api::InfoFilter,
    id: &str,
    time: i64,
    direction: &str,
) -> String {
    let mut value = json!({});
    if let Some(directory) = &filter.directory {
        value["directory"] = json!(directory);
    }
    if let Some(project) = &filter.project {
        value["project"] = json!(project);
    }
    if let Some(subpath) = &filter.subpath {
        value["subpath"] = json!(subpath);
    }
    if let Some(search) = &filter.search {
        value["search"] = json!(search);
    }
    if let Some(parent) = &filter.parent_id {
        value["parentID"] = match parent {
            Some(id) => json!(id),
            None => json!("null"),
        };
    }
    if let Some(archived) = filter.archived {
        value["archived"] = json!(if archived { "true" } else { "false" });
    }
    if let Some(order) = &filter.order {
        value["order"] = json!(order);
    }
    value["anchor"] = json!({ "id": id, "time": time, "direction": direction });
    b64url_encode(value.to_string().as_bytes())
}

fn cursor_str(value: &serde_json::Value, key: &str) -> Option<String> {
    value.get(key)?.as_str().map(str::to_owned)
}

fn decode_session_cursor(
    raw: &str,
) -> Result<devcode_core::session_api::InfoFilter, (u16, Option<serde_json::Value>)> {
    let bytes = b64url_decode(raw).map_err(|_| cursor_error())?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| cursor_error())?;
    let anchor = value.get("anchor").ok_or_else(cursor_error)?;
    let id = anchor
        .get("id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(cursor_error)?;
    let time = anchor
        .get("time")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(cursor_error)?;
    let direction = anchor
        .get("direction")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(cursor_error)?;
    if direction != "previous" && direction != "next" {
        return Err(cursor_error());
    }
    let mut filter = devcode_core::session_api::InfoFilter {
        directory: cursor_str(&value, "directory"),
        project: cursor_str(&value, "project"),
        subpath: cursor_str(&value, "subpath"),
        search: cursor_str(&value, "search"),
        order: cursor_str(&value, "order"),
        parent_id: None,
        archived: None,
        anchor: None,
        limit: 50,
    };
    if let Some(order) = &filter.order {
        if order != "asc" && order != "desc" {
            return Err(cursor_error());
        }
    }
    filter.parent_id = match value.get("parentID") {
        None => None,
        Some(serde_json::Value::String(value)) if value == "null" => Some(None),
        Some(serde_json::Value::String(id)) => Some(Some(id.clone())),
        Some(_) => return Err(cursor_error()),
    };
    filter.archived = match value.get("archived") {
        None => None,
        Some(serde_json::Value::String(value)) if value == "true" => Some(true),
        Some(serde_json::Value::String(value)) if value == "false" => Some(false),
        Some(_) => return Err(cursor_error()),
    };
    filter.anchor = Some(devcode_core::session_api::Anchor {
        id,
        time,
        direction,
    });
    Ok(filter)
}

fn query_session_filter(
    params: &[(String, String)],
    limit: i64,
) -> Result<devcode_core::session_api::InfoFilter, (u16, Option<serde_json::Value>)> {
    let parent_id = match qget(params, "parentID") {
        None => None,
        Some("null") => Some(None),
        Some(id) if id.starts_with("ses") => Some(Some(id.to_owned())),
        Some(_) => return Err(error(400, "InvalidRequestError", "Invalid parentID")),
    };
    let archived = match qget(params, "archived") {
        None => None,
        Some("true") => Some(true),
        Some("false") => Some(false),
        Some(_) => return Err(error(400, "InvalidRequestError", "Invalid archived")),
    };
    Ok(devcode_core::session_api::InfoFilter {
        directory: qget(params, "directory").map(str::to_owned),
        project: qget(params, "project").map(str::to_owned),
        subpath: qget(params, "subpath").map(str::to_owned),
        search: qget(params, "search").map(str::to_owned),
        order: query_order(params)?,
        parent_id,
        archived,
        anchor: None,
        limit,
    })
}

fn list_sessions(
    params: &[(String, String)],
    conn: &rusqlite::Connection,
) -> (u16, Option<serde_json::Value>) {
    let limit = match query_limit(params, 50, i64::MAX) {
        Ok(limit) => limit,
        Err(response) => return response,
    };
    let filter = match qget(params, "cursor") {
        Some(raw) => match decode_session_cursor(raw) {
            Ok(mut filter) => {
                filter.limit = limit;
                filter
            }
            Err(response) => return response,
        },
        None => match query_session_filter(params, limit) {
            Ok(filter) => filter,
            Err(response) => return response,
        },
    };
    let rows = match devcode_core::session_api::list_info(conn, &filter) {
        Ok(rows) => rows,
        Err(err) => {
            return error(
                500,
                "UnknownError",
                &format!("Failed to list sessions: {err}"),
            )
        }
    };
    let mut cursor = json!({});
    if let Some(first) = rows.first() {
        let direction = "previous";
        cursor["previous"] = json!(encode_session_cursor(
            &filter,
            &first.id,
            first.time_updated,
            direction
        ));
    }
    if let Some(last) = rows.last() {
        cursor["next"] = json!(encode_session_cursor(
            &filter,
            &last.id,
            last.time_updated,
            "next"
        ));
    }
    let data: Vec<serde_json::Value> = rows
        .iter()
        .map(devcode_core::session_api::public_json)
        .collect();
    (200, Some(json!({ "data": data, "cursor": cursor })))
}

fn json_body(body: &[u8]) -> Result<serde_json::Value, (u16, Option<serde_json::Value>)> {
    let value: serde_json::Value = serde_json::from_slice(body)
        .map_err(|_| error(400, "InvalidRequestError", "Request body must be JSON"))?;
    if !value.is_object() {
        return Err(error(
            400,
            "InvalidRequestError",
            "Request body must be a JSON object",
        ));
    }
    Ok(value)
}

fn optional_string(
    body: &serde_json::Value,
    key: &str,
) -> Result<Option<String>, (u16, Option<serde_json::Value>)> {
    match body.get(key) {
        None => Ok(None),
        Some(serde_json::Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(error(400, "InvalidRequestError", &format!("Invalid {key}"))),
    }
}

fn create_session(
    params: &[(String, String)],
    body: &[u8],
    conn: &mut rusqlite::Connection,
    now: i64,
) -> (u16, Option<serde_json::Value>) {
    let _ = params;
    let body = match json_body(body) {
        Ok(body) => body,
        Err(response) => return response,
    };
    let id = match optional_string(&body, "id") {
        Ok(Some(id)) if !id.starts_with("ses") => {
            return error(
                400,
                "InvalidRequestError",
                "Session ids must start with \"ses\"",
            )
        }
        Ok(Some(id)) => Some(id),
        Ok(None) => None,
        Err(response) => return response,
    };
    let parent_id = match optional_string(&body, "parentID") {
        Ok(Some(id)) if !id.starts_with("ses") => {
            return error(400, "InvalidRequestError", "parentID must be a session id")
        }
        Ok(Some(id)) => Some(id),
        Ok(None) => None,
        Err(response) => return response,
    };
    let title = match optional_string(&body, "title") {
        Ok(title) => title,
        Err(response) => return response,
    };
    let agent = match optional_string(&body, "agent") {
        Ok(agent) => agent,
        Err(response) => return response,
    };
    let metadata = match body.get("metadata") {
        None => None,
        Some(value) if value.is_object() => Some(value.clone()),
        Some(_) => return error(400, "InvalidRequestError", "metadata must be an object"),
    };
    let permissions = match body.get("permissions") {
        None => None,
        Some(value) if value.is_array() => Some(value.clone()),
        Some(_) => return error(400, "InvalidRequestError", "permissions must be an array"),
    };
    let model = match body.get("model") {
        None => None,
        Some(value) => {
            let object = match value.as_object() {
                Some(object) => object,
                None => return error(400, "InvalidRequestError", "model must be an object"),
            };
            let has = |key: &str| {
                object
                    .get(key)
                    .and_then(serde_json::Value::as_str)
                    .is_some()
            };
            if !has("id") || !has("providerID") {
                return error(
                    400,
                    "InvalidRequestError",
                    "model requires id and providerID",
                );
            }
            Some(value.clone())
        }
    };
    let (directory, workspace_id) = match body.get("location") {
        None => (None, None),
        Some(value) => {
            let directory = value
                .get("directory")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned);
            match directory {
                Some(directory) if std::path::Path::new(&directory).is_absolute() => {
                    let workspace_id = value
                        .get("workspaceID")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned);
                    (Some(directory), workspace_id)
                }
                Some(_) => {
                    return error(
                        400,
                        "InvalidRequestError",
                        "location.directory must be an absolute path",
                    )
                }
                None => return error(400, "InvalidRequestError", "location requires directory"),
            }
        }
    };
    match devcode_core::session_api::create_via_api(
        conn,
        devcode_core::session_api::CreateViaApi {
            id,
            parent_id,
            directory,
            workspace_id,
            title,
            agent,
            model,
            metadata,
            permissions,
            version: devcode_util::VERSION.to_owned(),
            now,
        },
    ) {
        Ok(info) => (
            200,
            Some(json!({ "data": devcode_core::session_api::public_json(&info) })),
        ),
        Err(devcode_core::session_api::CreateError::ParentNotFound(parent_id)) => {
            not_found(&parent_id)
        }
        Err(devcode_core::session_api::CreateError::Conflict(id)) => (
            409,
            Some(json!({
                "_tag": "ConflictError",
                "resource": id,
                "message": format!("Session already exists: {id}"),
            })),
        ),
        Err(devcode_core::session_api::CreateError::Database(err)) => error(
            500,
            "UnknownError",
            &format!("Failed to create session: {err}"),
        ),
    }
}

fn get_session(id: &str, conn: &rusqlite::Connection) -> (u16, Option<serde_json::Value>) {
    if !id.starts_with("ses") {
        return error(400, "InvalidRequestError", "Invalid session id");
    }
    match devcode_core::session_api::get_info(conn, id) {
        Ok(Some(info)) => (
            200,
            Some(json!({ "data": devcode_core::session_api::public_json(&info) })),
        ),
        Ok(None) => not_found(id),
        Err(err) => error(
            500,
            "UnknownError",
            &format!("Failed to read session: {err}"),
        ),
    }
}

fn delete_session(
    id: &str,
    conn: &mut rusqlite::Connection,
    now: i64,
) -> (u16, Option<serde_json::Value>) {
    if !id.starts_with("ses") {
        return error(400, "InvalidRequestError", "Invalid session id");
    }
    match devcode_core::session_api::delete_session(conn, id, now) {
        Ok(true) => (204, None),
        Ok(false) => not_found(id),
        Err(err) => error(
            500,
            "UnknownError",
            &format!("Failed to delete session: {err}"),
        ),
    }
}

fn update_session(
    id: &str,
    body: &[u8],
    conn: &mut rusqlite::Connection,
    now: i64,
) -> (u16, Option<serde_json::Value>) {
    if !id.starts_with("ses") {
        return error(400, "InvalidRequestError", "Invalid session id");
    }
    let body = match json_body(body) {
        Ok(body) => body,
        Err(response) => return response,
    };
    if let Some(title) = body.get("title") {
        match title.as_str() {
            Some(title) if !title.is_empty() => {}
            // The Bun handler generates a title for an empty string; that path needs a model, so it is not ported.
            Some(_) => {
                return error(
                    400,
                    "InvalidRequestError",
                    "title must be non-empty (or omit it)",
                )
            }
            None => return error(400, "InvalidRequestError", "title must be a string"),
        }
    }
    if let Some(metadata) = body.get("metadata") {
        if !metadata.is_object() {
            return error(400, "InvalidRequestError", "metadata must be an object");
        }
    }
    if let Some(permissions) = body.get("permissions") {
        if !permissions.is_array() {
            return error(400, "InvalidRequestError", "permissions must be an array");
        }
    }
    if let Some(archived) = body.get("archived") {
        if !archived.is_boolean() {
            return error(400, "InvalidRequestError", "archived must be a boolean");
        }
    }
    let apply = |result: rusqlite::Result<bool>| match result {
        Ok(true) => None,
        Ok(false) => Some(not_found(id)),
        Err(err) => Some(error(
            500,
            "UnknownError",
            &format!("Failed to update session: {err}"),
        )),
    };
    if let Some(title) = body.get("title").and_then(serde_json::Value::as_str) {
        if let Some(response) = apply(devcode_core::session_api::rename(conn, id, title, now)) {
            return response;
        }
    }
    if let Some(metadata) = body.get("metadata") {
        if let Some(response) = apply(devcode_core::session_api::set_metadata(
            conn, id, metadata, now,
        )) {
            return response;
        }
    }
    if let Some(permissions) = body.get("permissions") {
        if let Some(response) = apply(devcode_core::session_api::set_permissions(
            conn,
            id,
            permissions,
            now,
        )) {
            return response;
        }
    }
    if let Some(archived) = body.get("archived").and_then(serde_json::Value::as_bool) {
        if let Some(response) = apply(devcode_core::session_api::set_archived(
            conn, id, archived, now,
        )) {
            return response;
        }
    }
    (204, None)
}

fn prompt_session(
    id: &str,
    body: &[u8],
    conn: &mut rusqlite::Connection,
    _now: i64,
) -> (u16, Option<serde_json::Value>) {
    if !id.starts_with("ses") {
        return error(400, "InvalidRequestError", "Invalid session id");
    }
    let body = match json_body(body) {
        Ok(body) => body,
        Err(response) => return response,
    };
    let text = match body.get("text").and_then(serde_json::Value::as_str) {
        Some(text) if !text.is_empty() => text.to_owned(),
        Some(_) => return error(400, "InvalidRequestError", "text must be non-empty"),
        None => return error(400, "InvalidRequestError", "text is required"),
    };
    let explicit_id = match optional_string(&body, "id") {
        Ok(Some(message_id)) if !message_id.starts_with("msg") => {
            return error(400, "InvalidRequestError", "id must be a message id")
        }
        Ok(message_id) => message_id,
        Err(response) => return response,
    };
    let delivery = match optional_string(&body, "delivery") {
        Ok(None) => "steer".to_owned(),
        Ok(Some(delivery)) if delivery == "steer" || delivery == "queue" => delivery,
        Ok(Some(_)) => {
            return error(
                400,
                "InvalidRequestError",
                "delivery must be \"steer\" or \"queue\"",
            )
        }
        Err(response) => return response,
    };
    let resume = match body.get("resume") {
        None => true,
        Some(serde_json::Value::Bool(value)) => *value,
        Some(_) => return error(400, "InvalidRequestError", "resume must be a boolean"),
    };
    let model_raw = match optional_string(&body, "model") {
        Ok(Some(model)) => Some(model),
        Ok(None) => std::env::var("DEVCODE_MODEL")
            .ok()
            .filter(|value| !value.trim().is_empty()),
        Err(response) => return response,
    };
    let model = match model_raw.as_deref() {
        Some(raw) => match devcode_ai::parse_model(raw) {
            Ok(model) => Some(model),
            Err(message) => return error(400, "InvalidRequestError", &message),
        },
        None => None,
    };
    let mut extra = serde_json::Map::new();
    for key in ["files", "agents", "skills", "metadata"] {
        if let Some(value) = body.get(key) {
            let valid = if key == "metadata" {
                value.is_object()
            } else {
                value.is_array()
            };
            if !valid {
                let want = if key == "metadata" {
                    "an object"
                } else {
                    "an array"
                };
                return error(400, "InvalidRequestError", &format!("{key} must be {want}"));
            }
            extra.insert(key.to_owned(), value.clone());
        }
    }
    match devcode_core::session_api::get_info(conn, id) {
        Ok(Some(_)) => {}
        Ok(None) => return not_found(id),
        Err(err) => {
            return error(
                500,
                "UnknownError",
                &format!("Failed to read session: {err}"),
            )
        }
    }
    let extra = serde_json::Value::Object(extra);
    if !resume {
        return match devcode_runner::admit(
            conn,
            id,
            &text,
            &delivery,
            explicit_id.as_deref(),
            Some(&extra),
        ) {
            Ok(admitted) => (200, Some(json!({ "data": admitted.item }))),
            Err(message) => error(500, "UnknownError", &message),
        };
    }
    let Some(model) = model else {
        return error(
            400,
            "InvalidRequestError",
            "no model selected; pass model or set DEVCODE_MODEL",
        );
    };
    if !running_insert(id) {
        return error(
            409,
            "ConflictError",
            "Session already has a run in progress",
        );
    }
    match devcode_runner::admit(
        conn,
        id,
        &text,
        &delivery,
        explicit_id.as_deref(),
        Some(&extra),
    ) {
        Ok(admitted) => {
            let paths = devcode_util::paths("devcode");
            let session_id = id.to_owned();
            std::thread::spawn(move || {
                let mut sink = |_: &str| {};
                let mut tool = |_: &str, _: &str| {};
                let _ = devcode_runner::run_admitted(
                    &paths,
                    &model,
                    &session_id,
                    32,
                    &mut sink,
                    &mut tool,
                );
                running_remove(&session_id);
            });
            (200, Some(json!({ "data": admitted.item })))
        }
        Err(message) => {
            running_remove(id);
            error(500, "UnknownError", &message)
        }
    }
}

fn encode_message_cursor(id: &str, order: &str, direction: &str) -> String {
    b64url_encode(
        json!({ "id": id, "order": order, "direction": direction })
            .to_string()
            .as_bytes(),
    )
}

fn list_messages(
    id: &str,
    params: &[(String, String)],
    conn: &rusqlite::Connection,
) -> (u16, Option<serde_json::Value>) {
    if !id.starts_with("ses") {
        return error(400, "InvalidRequestError", "Invalid session id");
    }
    let limit = match query_limit(params, 50, 200) {
        Ok(limit) => limit,
        Err(response) => return response,
    };
    let cursor: Option<(String, String, String)> = match qget(params, "cursor") {
        Some(raw) => {
            if qget(params, "order").is_some() {
                return error(
                    400,
                    "InvalidCursorError",
                    "Cursor cannot be combined with order",
                );
            }
            let value: Option<serde_json::Value> = b64url_decode(raw)
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok());
            let parsed = value.and_then(|value| {
                let id = value.get("id")?.as_str()?.to_owned();
                let order = value.get("order")?.as_str()?.to_owned();
                let direction = value.get("direction")?.as_str()?.to_owned();
                if !matches!(order.as_str(), "asc" | "desc")
                    || !matches!(direction.as_str(), "previous" | "next")
                {
                    return None;
                }
                Some((id, order, direction))
            });
            match parsed {
                Some(cursor) => Some(cursor),
                None => return cursor_error(),
            }
        }
        None => None,
    };
    let requested_order = match &cursor {
        Some((_, order, _)) => order.clone(),
        None => match query_order(params) {
            Ok(Some(order)) => order,
            Ok(None) => "desc".to_owned(),
            Err(response) => return response,
        },
    };
    let type_filter = match qget(params, "type") {
        None => None,
        Some(type_filter) if MESSAGE_TYPES.contains(&type_filter) => Some(type_filter.to_owned()),
        Some(_) => return error(400, "InvalidRequestError", "Invalid message type"),
    };
    match devcode_core::session_api::get_info(conn, id) {
        Ok(Some(_)) => {}
        Ok(None) => return not_found(id),
        Err(err) => {
            return error(
                500,
                "UnknownError",
                &format!("Failed to read session: {err}"),
            )
        }
    }
    let cursor = cursor.map(|(id, _, direction)| (id, direction));
    let rows = match devcode_core::session_message::messages_page(
        conn,
        id,
        limit,
        &requested_order,
        type_filter.as_deref(),
        cursor
            .as_ref()
            .map(|(id, direction)| (id.as_str(), direction.as_str())),
    ) {
        Ok(rows) => rows,
        Err(err) => {
            return error(
                500,
                "UnknownError",
                &format!("Failed to list messages: {err}"),
            )
        }
    };
    let data: Vec<serde_json::Value> = rows
        .iter()
        .map(devcode_core::session_message::wire)
        .collect();
    let mut cursor_json = json!({});
    if let Some(first) = rows.first() {
        cursor_json["previous"] = json!(encode_message_cursor(
            &first.id,
            &requested_order,
            "previous"
        ));
    }
    if let Some(last) = rows.last() {
        cursor_json["next"] = json!(encode_message_cursor(&last.id, &requested_order, "next"));
    }
    (200, Some(json!({ "data": data, "cursor": cursor_json })))
}

fn route(
    method: &str,
    path: &str,
    query: &str,
    body: &[u8],
    conn: &mut rusqlite::Connection,
    info: &serde_json::Value,
    now: i64,
) -> (u16, Option<serde_json::Value>) {
    let owned: Vec<String> = path
        .split('/')
        .filter(|part| !part.is_empty())
        .map(percent_decode)
        .collect();
    let segments: Vec<&str> = owned.iter().map(String::as_str).collect();
    let params = parse_query(query);
    match (method, segments.as_slice()) {
        ("GET", ["api", "info"]) => (200, Some(info.clone())),
        ("GET", ["openapi.json"]) => (200, Some(openapi())),
        ("GET", ["api", "session"]) => list_sessions(&params, conn),
        ("POST", ["api", "session"]) => create_session(&params, body, conn, now),
        ("GET", ["api", "session", id]) => get_session(id, conn),
        ("DELETE", ["api", "session", id]) => delete_session(id, conn, now),
        ("PATCH", ["api", "session", id]) => update_session(id, body, conn, now),
        ("POST", ["api", "session", id, "prompt"]) => prompt_session(id, body, conn, now),
        ("GET", ["api", "session", id, "message"]) => list_messages(id, &params, conn),
        ("GET", ["api", "plugin"]) => (200, Some(json!({ "data": plugin_list() }))),
        _ => (404, Some(json!({"message": "Not Found"}))),
    }
}

fn respond(request: Request, status: u16, body: serde_json::Value) {
    let payload = serde_json::to_string(&body).unwrap_or_else(|_| "{}".to_string());
    let response = Response::from_string(payload)
        .with_status_code(StatusCode(status))
        .with_header(Header::from_bytes("content-type", "application/json").unwrap());
    let _ = request.respond(response);
}

fn authorized(request: &Request, password: &str) -> bool {
    let Some(header) = request
        .headers()
        .iter()
        .find(|header| header.field.equiv("authorization"))
    else {
        return false;
    };
    let Ok(value) = std::str::from_utf8(header.value.as_bytes()) else {
        return false;
    };
    let Some(encoded) = value.strip_prefix("Basic ") else {
        return false;
    };
    let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(encoded.trim()) else {
        return false;
    };
    let Ok(credentials) = std::str::from_utf8(&decoded) else {
        return false;
    };
    let Some((user, pass)) = credentials.split_once(':') else {
        return false;
    };
    user == svc::AUTH_USERNAME && svc::constant_time_eq(pass, password)
}

fn openapi() -> serde_json::Value {
    json!({
        "openapi": "3.1.0",
        "info": {
            "title": "devcode HttpApi",
            "version": devcode_util::VERSION,
            "description": "Rust port of the DEVcode HttpApi surface. Served so far: /api/info, /api/session, /api/session/{sessionID}, /api/session/{sessionID}/message.",
        },
        "paths": {
            "/api/info": {
                "get": {
                    "tags": ["server"],
                    "summary": "Service information",
                    "responses": { "200": { "description": "Service information" } },
                },
            },
            "/api/session": {
                "get": {
                    "tags": ["session"],
                    "summary": "List sessions",
                    "responses": { "200": { "description": "Sessions with cursor" } },
                },
                "post": {
                    "tags": ["session"],
                    "summary": "Create session",
                    "responses": { "200": { "description": "Created session" } },
                },
            },
            "/api/session/{sessionID}": {
                "get": {
                    "tags": ["session"],
                    "summary": "Get session",
                    "responses": { "200": { "description": "Session" } },
                },
                "patch": {
                    "tags": ["session"],
                    "summary": "Update session",
                    "responses": { "204": { "description": "Updated" } },
                },
                "delete": {
                    "tags": ["session"],
                    "summary": "Delete session",
                    "responses": { "204": { "description": "Deleted" } },
                },
            },
            "/api/plugin": {
                "get": {
                    "tags": ["plugin"],
                    "summary": "List plugins",
                    "description": "Enabled server plugins and their current status. Native Rust plugins only.",
                    "responses": { "200": { "description": "Enabled plugins" } },
                },
            },
            "/api/session/{sessionID}/prompt": {
                "post": {
                    "tags": ["session"],
                    "summary": "Send message",
                    "description": "Durably admit one session input; with resume (default) the agent loop runs in the background. 409 when the session already has a run in progress.",
                    "responses": { "200": { "description": "Admitted inbox item" } },
                },
            },
            "/api/session/{sessionID}/message": {
                "get": {
                    "tags": ["session"],
                    "summary": "Get session messages",
                    "responses": { "200": { "description": "Messages with cursor" } },
                },
            },
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use devcode_core::session_message;
    use serde_json::{json, Value};
    use std::path::PathBuf;

    struct TestDb {
        dir: PathBuf,
        db: devcode_core::Database,
    }

    impl Drop for TestDb {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn temp_db(tag: &str) -> TestDb {
        let dir = std::env::temp_dir().join(format!("devcode-http-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = devcode_core::Database::open(&dir.join("devcode.db")).unwrap();
        TestDb { dir, db }
    }

    fn call(
        conn: &mut rusqlite::Connection,
        method: &str,
        path: &str,
        query: &str,
        body: &str,
        now: i64,
    ) -> (u16, Option<Value>) {
        route(
            method,
            path,
            query,
            body.as_bytes(),
            conn,
            &json!({"version": "test"}),
            now,
        )
    }

    #[test]
    fn create_get_list_patch_delete_roundtrip() {
        let mut t = temp_db("roundtrip");
        let c = &mut t.db.conn;
        let (status, body) = call(c, "POST", "/api/session", "", r#"{"title": "hello"}"#, 1000);
        assert_eq!(status, 200);
        let id = body.unwrap()["data"]["id"].as_str().unwrap().to_owned();
        assert!(id.starts_with("ses_"));

        let (status, body) = call(c, "GET", &format!("/api/session/{id}"), "", "", 1001);
        assert_eq!(status, 200);
        assert_eq!(body.unwrap()["data"]["title"], "hello");

        let (status, body) = call(c, "GET", "/api/session", "", "", 1002);
        assert_eq!(status, 200);
        assert_eq!(body.unwrap()["data"].as_array().unwrap().len(), 1);

        let (status, _) = call(
            c,
            "PATCH",
            &format!("/api/session/{id}"),
            "",
            r#"{"title":"renamed"}"#,
            1003,
        );
        assert_eq!(status, 204);
        let (_, body) = call(c, "GET", &format!("/api/session/{id}"), "", "", 1004);
        assert_eq!(body.unwrap()["data"]["title"], "renamed");

        let (status, _) = call(
            c,
            "PATCH",
            &format!("/api/session/{id}"),
            "",
            r#"{"archived":true}"#,
            1005,
        );
        assert_eq!(status, 204);
        let (_, body) = call(c, "GET", "/api/session", "archived=true", "", 1006);
        assert_eq!(body.as_ref().unwrap()["data"].as_array().unwrap().len(), 1);
        let (_, body) = call(c, "GET", "/api/session", "archived=false", "", 1007);
        assert_eq!(body.as_ref().unwrap()["data"].as_array().unwrap().len(), 0);

        let (status, _) = call(c, "DELETE", &format!("/api/session/{id}"), "", "", 1008);
        assert_eq!(status, 204);
        let (status, _) = call(c, "GET", &format!("/api/session/{id}"), "", "", 1009);
        assert_eq!(status, 404);
    }

    #[test]
    fn list_cursor_pages_newest_first() {
        let mut t = temp_db("pages");
        let c = &mut t.db.conn;
        for (title, now) in [("a", 1000_i64), ("b", 2000), ("c", 3000)] {
            let (status, _) = call(
                c,
                "POST",
                "/api/session",
                "",
                &format!(r#"{{"title":"{title}"}}"#),
                now,
            );
            assert_eq!(status, 200);
        }
        let (_, body) = call(c, "GET", "/api/session", "limit=2", "", 4000);
        let body = body.unwrap();
        let data = body["data"].as_array().unwrap();
        assert_eq!(data.len(), 2);
        assert_eq!(data[0]["title"], "c");
        assert_eq!(data[1]["title"], "b");
        let next = body["cursor"]["next"].as_str().unwrap().to_owned();
        let (_, body) = call(
            c,
            "GET",
            "/api/session",
            &format!("limit=2&cursor={next}"),
            "",
            4001,
        );
        let data = body.unwrap()["data"].as_array().unwrap().clone();
        assert_eq!(data.len(), 1);
        assert_eq!(data[0]["title"], "a");
    }

    #[test]
    fn message_list_wire_and_cursor() {
        let mut t = temp_db("messages");
        let c = &mut t.db.conn;
        let (status, body) = call(c, "POST", "/api/session", "", "{}", 1000);
        assert_eq!(status, 200);
        let id = body.unwrap()["data"]["id"].as_str().unwrap().to_owned();
        session_message::insert_message(
            c,
            "msg_a",
            &id,
            "user",
            1,
            1100,
            json!({"text":"hi","time":{"created":1100}}),
        )
        .unwrap();
        session_message::insert_assistant(
            c,
            "msg_b",
            &id,
            2,
            "build",
            &json!({"id":"m","providerID":"p"}),
            1200,
        )
        .unwrap();
        session_message::replace_parts(
            c,
            "msg_b",
            &id,
            &[json!({"type":"text","text":"hello"})],
            1200,
            1200,
        )
        .unwrap();

        let (status, body) = call(
            c,
            "GET",
            &format!("/api/session/{id}/message"),
            "order=asc",
            "",
            2000,
        );
        assert_eq!(status, 200);
        let body = body.unwrap();
        let data = body["data"].as_array().unwrap();
        assert_eq!(data.len(), 2);
        assert_eq!(data[0]["type"], "user");
        assert_eq!(data[1]["content"][0]["text"], "hello");

        let (_, body) = call(
            c,
            "GET",
            &format!("/api/session/{id}/message"),
            "limit=1",
            "",
            2001,
        );
        let body = body.unwrap();
        let first_id = body["data"][0]["id"].as_str().unwrap().to_owned();
        let next = body["cursor"]["next"].as_str().unwrap().to_owned();
        let (_, body) = call(
            c,
            "GET",
            &format!("/api/session/{id}/message"),
            &format!("limit=1&cursor={next}"),
            "",
            2002,
        );
        let data = body.unwrap()["data"].as_array().unwrap().clone();
        assert_eq!(data.len(), 1);
        assert_ne!(data[0]["id"], first_id);

        let (_, body) = call(
            c,
            "GET",
            &format!("/api/session/{id}/message"),
            "type=user",
            "",
            2003,
        );
        assert_eq!(body.unwrap()["data"].as_array().unwrap().len(), 1);

        let (status, _) = call(
            c,
            "GET",
            &format!("/api/session/{id}/message"),
            "type=nope",
            "",
            2004,
        );
        assert_eq!(status, 400);

        let (status, _) = call(
            c,
            "GET",
            &format!("/api/session/{id}/message"),
            "cursor=!!!",
            "",
            2005,
        );
        assert_eq!(status, 400);

        let (status, _) = call(
            c,
            "GET",
            &format!("/api/session/{id}/message"),
            &format!("cursor={next}&order=asc"),
            "",
            2006,
        );
        assert_eq!(status, 400);
    }

    #[test]
    fn child_sessions_cascade_on_delete() {
        let mut t = temp_db("cascade");
        let c = &mut t.db.conn;
        let (_, body) = call(c, "POST", "/api/session", "", r#"{"title":"parent"}"#, 1000);
        let parent = body.unwrap()["data"]["id"].as_str().unwrap().to_owned();
        let (_, body) = call(
            c,
            "POST",
            "/api/session",
            "",
            &format!(r#"{{"parentID":"{parent}","title":"child"}}"#),
            1001,
        );
        let child = body.unwrap()["data"]["id"].as_str().unwrap().to_owned();
        let (status, _) = call(c, "DELETE", &format!("/api/session/{parent}"), "", "", 1002);
        assert_eq!(status, 204);
        let (status, _) = call(c, "GET", &format!("/api/session/{child}"), "", "", 1003);
        assert_eq!(status, 404);
    }

    #[test]
    fn prompt_endpoint_admits_and_validates() {
        let previous_model = std::env::var("DEVCODE_MODEL").ok();
        std::env::remove_var("DEVCODE_MODEL");
        let mut t = temp_db("prompt");
        let c = &mut t.db.conn;
        let (_, body) = call(c, "POST", "/api/session", "", r#"{"title":"p"}"#, 1000);
        let id = body.unwrap()["data"]["id"].as_str().unwrap().to_owned();

        // admit-only: no model required
        let (status, body) = call(
            c,
            "POST",
            &format!("/api/session/{id}/prompt"),
            "",
            r#"{"text":"hello","resume":false,"files":[{"name":"a.rs"}]}"#,
            1001,
        );
        assert_eq!(status, 200);
        let data = &body.unwrap()["data"];
        assert_eq!(data["type"], "user");
        assert_eq!(data["payload"]["text"], "hello");
        assert_eq!(data["payload"]["files"][0]["name"], "a.rs");
        assert_eq!(data["delivery"], "steer");
        assert!(data["id"].as_str().unwrap().starts_with("msg_"));
        assert_eq!(data["sessionID"], id);
        assert!(data["time"]["created"].is_number());

        // explicit message id must be a message id
        let (status, _) = call(
            c,
            "POST",
            &format!("/api/session/{id}/prompt"),
            "",
            r#"{"text":"hi","resume":false,"id":"ses_nope"}"#,
            1002,
        );
        assert_eq!(status, 400);

        // resume requires a model when DEVCODE_MODEL is unset
        let (status, body) = call(
            c,
            "POST",
            &format!("/api/session/{id}/prompt"),
            "",
            r#"{"text":"hi"}"#,
            1003,
        );
        assert_eq!(status, 400);
        assert_eq!(body.unwrap()["_tag"], "InvalidRequestError");

        // unparseable model
        let (status, _) = call(
            c,
            "POST",
            &format!("/api/session/{id}/prompt"),
            "",
            r#"{"text":"hi","model":"nope"}"#,
            1004,
        );
        assert_eq!(status, 400);

        // empty text, bad delivery, bad extras
        for body_str in [
            r#"{"text":""}"#,
            r#"{"text":"hi","delivery":"now"}"#,
            r#"{"text":"hi","resume":"yes"}"#,
            r#"{"text":"hi","metadata":[]}"#,
            r#"{"text":"hi","files":{}}"#,
        ] {
            let (status, _) = call(
                c,
                "POST",
                &format!("/api/session/{id}/prompt"),
                "",
                body_str,
                1005,
            );
            assert_eq!(status, 400, "body {body_str}");
        }

        // unknown session
        let (status, _) = call(
            c,
            "POST",
            "/api/session/ses_missing/prompt",
            "",
            r#"{"text":"hi","resume":false}"#,
            1006,
        );
        assert_eq!(status, 404);

        // the admitted prompt is visible in the message list
        let (_, body) = call(
            c,
            "GET",
            &format!("/api/session/{id}/message"),
            "",
            "",
            1007,
        );
        let data = body.unwrap()["data"].as_array().unwrap().clone();
        assert_eq!(data.len(), 1);
        assert_eq!(data[0]["type"], "user");
        assert_eq!(data[0]["text"], "hello");
        assert_eq!(data[0]["files"][0]["name"], "a.rs");

        if let Some(value) = previous_model {
            std::env::set_var("DEVCODE_MODEL", value);
        }
    }

    #[test]
    fn plugin_list_endpoint_responds() {
        let mut t = temp_db("plugin-list");
        let c = &mut t.db.conn;
        let (status, body) = call(c, "GET", "/api/plugin", "", "", 1000);
        assert_eq!(status, 200);
        assert!(body.unwrap()["data"].is_array());
    }

    #[test]
    fn explicit_id_conflict_and_unknown_parent() {
        let mut t = temp_db("conflict");
        let c = &mut t.db.conn;
        let (status, _) = call(
            c,
            "POST",
            "/api/session",
            "",
            r#"{"id":"ses_explicit1"}"#,
            1000,
        );
        assert_eq!(status, 200);
        let (status, body) = call(
            c,
            "POST",
            "/api/session",
            "",
            r#"{"id":"ses_explicit1"}"#,
            1001,
        );
        assert_eq!(status, 409);
        assert_eq!(body.unwrap()["_tag"], "ConflictError");
        let (status, _) = call(
            c,
            "POST",
            "/api/session",
            "",
            r#"{"parentID":"ses_missing"}"#,
            1002,
        );
        assert_eq!(status, 404);
    }
}
