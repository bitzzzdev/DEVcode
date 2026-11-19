//! End-to-end OAuth flow for remote MCP servers against a loopback fake
//! (RFC 9728 discovery, refresh_token grant, Bearer injection on MCP requests).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use devcode_mcp::oauth::{CredentialStore, StoredOAuth};
use devcode_mcp::{Connection, RemoteConfig, ServerConfig};

struct FakeServer {
    base: String,
    seen_authorization: Arc<Mutex<Vec<String>>>,
    expected_token: Arc<Mutex<String>>,
}

fn start_fake() -> FakeServer {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen_authorization = Arc::new(Mutex::new(Vec::new()));
    let expected_token = Arc::new(Mutex::new(String::new()));
    let server_base = base.clone();
    let thread_seen = seen_authorization.clone();
    let thread_expected = expected_token.clone();
    std::thread::spawn(move || {
        let server = tiny_http::Server::from_listener(listener, None).unwrap();
        for mut request in server.incoming_requests() {
            let method = request.method().to_string();
            let url = request.url().to_string();
            let mut body = String::new();
            if method == "POST" {
                request.as_reader().read_to_string(&mut body).unwrap();
            }
            let authorization = request
                .headers()
                .iter()
                .find(|header| header.field.equiv("Authorization"))
                .map(|header| header.value.as_str().to_string());
            let (status, payload) = match (method.as_str(), url.as_str()) {
                ("GET", "/.well-known/oauth-protected-resource/mcp")
                | ("GET", "/.well-known/oauth-protected-resource") => (
                    200,
                    serde_json::json!({
                        "resource": format!("{server_base}/mcp"),
                        "authorization_servers": [server_base],
                        "scopes_supported": ["mcp:connect"],
                    }),
                ),
                ("GET", "/.well-known/oauth-authorization-server") => (
                    200,
                    serde_json::json!({
                        "issuer": server_base,
                        "authorization_endpoint": format!("{server_base}/authorize"),
                        "token_endpoint": format!("{server_base}/token"),
                        "registration_endpoint": format!("{server_base}/register"),
                    }),
                ),
                ("POST", "/token") => {
                    assert!(body.contains("grant_type=refresh_token"), "token grant: {body}");
                    assert!(body.contains("refresh_token=stored-refresh"), "refresh token: {body}");
                    (
                        200,
                        serde_json::json!({
                            "access_token": "fresh-access",
                            "refresh_token": "fresh-refresh",
                            "expires_in": 3600,
                            "token_type": "Bearer",
                        }),
                    )
                }
                ("POST", "/mcp") => {
                    if let Some(header) = &authorization {
                        thread_seen.lock().unwrap().push(header.clone());
                    }
                    let expected = thread_expected.lock().unwrap().clone();
                    if authorization.as_deref() != Some(expected.as_str()) {
                        (401, serde_json::json!({ "error": "invalid_token" }))
                    } else {
                        let message: serde_json::Value = serde_json::from_str(&body).unwrap();
                        let id = message.get("id").cloned().unwrap_or(serde_json::Value::Null);
                        let mcp_method = message["method"].as_str().unwrap_or("");
                        if mcp_method == "notifications/initialized" {
                            let _ = request.respond(tiny_http::Response::empty(202));
                            continue;
                        }
                        let result = match mcp_method {
                            "initialize" => serde_json::json!({
                                "protocolVersion": "2025-03-26",
                                "capabilities": { "tools": {} },
                                "serverInfo": { "name": "fake", "version": "0.0.0" },
                            }),
                            "tools/list" => serde_json::json!({
                                "tools": [{ "name": "ping", "description": "ping", "inputSchema": { "type": "object" } }],
                            }),
                            _ => serde_json::json!({}),
                        };
                        (
                            200,
                            serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result }),
                        )
                    }
                }
                _ => (404, serde_json::json!({ "error": "not found" })),
            };
            let mut response = tiny_http::Response::from_string(payload.to_string())
                .with_status_code(tiny_http::StatusCode(status));
            response.add_header(
                tiny_http::Header::from_bytes(&b"content-type"[..], &b"application/json"[..]).unwrap(),
            );
            response.add_header(
                tiny_http::Header::from_bytes(&b"mcp-session-id"[..], &b"session-1"[..]).unwrap(),
            );
            request.respond(response).ok();
        }
    });
    // let the server thread start
    std::thread::sleep(Duration::from_millis(100));
    FakeServer {
        base,
        seen_authorization,
        expected_token,
    }
}

fn temp_db(tag: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("devcode-mcp-oauth-test-{tag}-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);
    path
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

fn seed(db_path: &std::path::Path, server: &str, url: &str, access: &str, expires: u64, refresh: &str) {
    let store = CredentialStore::open(&db_path.to_string_lossy()).unwrap();
    let integration_id = CredentialStore::integration_id(server, url);
    store
        .save(
            &integration_id,
            "default",
            &StoredOAuth {
                kind: "oauth".into(),
                method_id: integration_id.clone(),
                access: access.into(),
                refresh: refresh.into(),
                expires,
                metadata: BTreeMap::from([("client".to_owned(), serde_json::json!({ "client_id": "fake-client" }))]),
            },
        )
        .unwrap();
}

#[test]
fn connects_with_valid_stored_token() {
    let server = start_fake();
    let url = format!("{}/mcp", server.base);
    let db = temp_db("valid");
    seed(&db, "figma", &url, "valid-access", now_ms() + 3_600_000, "stored-refresh");
    *server.expected_token.lock().unwrap() = "Bearer valid-access".to_string();
    let config = ServerConfig::Remote(RemoteConfig {
        url: url.clone(),
        headers: BTreeMap::new(),
        needs_oauth: true,
        disabled: false,
    });
    let timeout = Duration::from_secs(5);
    let mut connection = Connection::connect("figma", &config, timeout, &db).expect("connect");
    let tools = connection.list_tools(timeout).expect("list tools");
    assert_eq!(tools[0].name, "ping");
    let seen = server.seen_authorization.lock().unwrap().clone();
    assert!(seen.iter().all(|value| value == "Bearer valid-access"), "authorizations: {seen:?}");
    assert!(!seen.is_empty());
    let _ = std::fs::remove_file(&db);
}

#[test]
fn expired_token_is_refreshed_before_connect() {
    let server = start_fake();
    let url = format!("{}/mcp", server.base);
    let db = temp_db("refresh");
    seed(&db, "figma", &url, "stale-access", now_ms() - 1000, "stored-refresh");
    *server.expected_token.lock().unwrap() = "Bearer fresh-access".to_string();
    let config = ServerConfig::Remote(RemoteConfig {
        url: url.clone(),
        headers: BTreeMap::new(),
        needs_oauth: true,
        disabled: false,
    });
    let timeout = Duration::from_secs(5);
    let mut connection = Connection::connect("figma", &config, timeout, &db).expect("connect");
    let tools = connection.list_tools(timeout).expect("list tools");
    assert_eq!(tools[0].name, "ping");
    let seen = server.seen_authorization.lock().unwrap().clone();
    assert!(seen.iter().all(|value| value == "Bearer fresh-access"), "authorizations: {seen:?}");
    // refresh must have persisted the new token, replacing the stale one
    let integration_id = CredentialStore::integration_id("figma", &url);
    let store = CredentialStore::open(&db.to_string_lossy()).unwrap();
    let stored = store.get(&integration_id).unwrap().unwrap();
    assert_eq!(stored.access, "fresh-access");
    assert_eq!(stored.refresh, "fresh-refresh");
    let _ = std::fs::remove_file(&db);
}
