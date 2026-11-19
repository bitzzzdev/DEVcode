use std::collections::BTreeMap;
use std::time::Duration;

use devcode_mcp::{
    load_config, namespaced_tool_name, strip_jsonc, Connection, McpConfig, ServerConfig,
};

fn fixture_path() -> String {
    env!("CARGO_BIN_EXE_mcp_fixture").to_owned()
}

#[test]
fn stdio_handshake_tools_list_and_call() {
    let config = ServerConfig::Local(devcode_mcp::LocalConfig {
        command: vec![fixture_path()],
        cwd: None,
        environment: BTreeMap::new(),
        disabled: false,
    });
    let timeout = Duration::from_secs(5);
    let db = std::path::Path::new(":memory:");
    let mut connection = Connection::connect("fixture", &config, timeout, db).expect("connect");
    let tools = connection.list_tools(timeout).expect("list tools");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "echo");
    let result = connection
        .call_tool("echo", &serde_json::json!({ "text": "hello" }), timeout)
        .expect("call tool");
    assert!(result.contains("hello"), "unexpected tool output: {result}");
}

#[test]
fn jsonc_stripping_keeps_strings_and_drops_comments() {
    let input = r#"{
        // line comment
        "mcp": { "servers": { "a": { "type": "local", "command": ["x"] } } }, // trailing
        /* block
           comment */
        "url": "http://example.com//not-a-comment"
    }"#;
    let stripped = strip_jsonc(input);
    assert!(stripped.contains("http://example.com//not-a-comment"));
    assert!(!stripped.contains("line comment"));
    let value: serde_json::Value = serde_json::from_str(&stripped).unwrap();
    assert_eq!(value["mcp"]["servers"]["a"]["type"], "local");
}

#[test]
fn load_config_reads_local_and_remote_and_disabled() {
    let dir = std::env::temp_dir().join(format!("devcode-mcp-config-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("devcode.jsonc");
    std::fs::write(
        &path,
        r#"{
        "mcp": { "servers": {
            "fs": { "type": "local", "command": ["server-fs"], "disabled": false },
            "remote": { "type": "remote", "url": "https://example.com/mcp", "headers": { "x-key": "v" } },
            "oauth": { "type": "remote", "url": "https://example.com/mcp", "oauth": {} },
            "off": { "type": "local", "command": ["x"], "disabled": true }
        } }
    }"#,
    )
    .unwrap();
    let config: McpConfig = load_config(&path).unwrap();
    assert_eq!(config.servers.len(), 4);
    match &config.servers["fs"] {
        ServerConfig::Local(local) => assert_eq!(local.command, vec!["server-fs"]),
        _ => panic!("fs should be local"),
    }
    match &config.servers["remote"] {
        ServerConfig::Remote(remote) => {
            assert_eq!(remote.url, "https://example.com/mcp");
            assert!(!remote.needs_oauth);
            assert_eq!(remote.headers["x-key"], "v");
        }
        _ => panic!("remote should be remote"),
    }
    match &config.servers["oauth"] {
        ServerConfig::Remote(remote) => assert!(remote.needs_oauth),
        _ => panic!("oauth should be remote"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stdio_resources_list_templates_read() {
    let config = ServerConfig::Local(devcode_mcp::LocalConfig {
        command: vec![fixture_path()],
        cwd: None,
        environment: BTreeMap::new(),
        disabled: false,
    });
    let timeout = Duration::from_secs(5);
    let db = std::path::Path::new(":memory:");
    let mut connection = Connection::connect("fixture", &config, timeout, db).expect("connect");
    assert!(connection.resources_advertised());
    let resources = connection.list_resources(timeout).expect("resources/list");
    assert_eq!(resources.len(), 1);
    assert_eq!(resources[0]["uri"], "fixture://demo");
    assert_eq!(resources[0]["name"], "demo");
    let templates = connection
        .list_resource_templates(timeout)
        .expect("resources/templates/list");
    assert_eq!(templates.len(), 1);
    assert_eq!(templates[0]["uriTemplate"], "fixture://item/{id}");
    let contents = connection
        .read_resource("fixture://item/42", timeout)
        .expect("resources/read");
    assert_eq!(contents[0]["text"], "item 42");
    // unknown resource uri -> JSON-RPC error from the server
    let error = connection.read_resource("fixture://nope", timeout);
    assert!(error.is_err());
}

#[test]
fn runtime_resource_catalog_and_read() {
    let mut servers = BTreeMap::new();
    servers.insert(
        "fixture".to_string(),
        ServerConfig::Local(devcode_mcp::LocalConfig {
            command: vec![fixture_path()],
            cwd: None,
            environment: BTreeMap::new(),
            disabled: false,
        }),
    );
    servers.insert(
        "broken".to_string(),
        ServerConfig::Local(devcode_mcp::LocalConfig {
            command: vec!["devcode-mcp-fixture-does-not-exist".to_string()],
            cwd: None,
            environment: BTreeMap::new(),
            disabled: false,
        }),
    );
    let timeout = Duration::from_secs(5);
    let db = std::path::Path::new(":memory:");
    let mut runtime = devcode_mcp::Runtime::connect_all(&servers, timeout, db);

    // Merged across connected servers; the broken one is skipped.
    let catalog = runtime.resource_catalog(None, timeout).expect("merged catalog");
    let resources = catalog["resources"].as_array().unwrap();
    assert_eq!(resources.len(), 1);
    assert_eq!(resources[0]["server"], "fixture");
    assert_eq!(resources[0]["uri"], "fixture://demo");
    assert_eq!(resources[0]["description"], "fixture document");
    assert_eq!(resources[0]["mimeType"], "text/plain");
    let templates = catalog["templates"].as_array().unwrap();
    assert_eq!(templates.len(), 1);
    assert_eq!(templates[0]["server"], "fixture");
    assert_eq!(templates[0]["uriTemplate"], "fixture://item/{id}");

    // Filtered to one server.
    let catalog = runtime.resource_catalog(Some("fixture"), timeout).expect("filtered catalog");
    assert_eq!(catalog["resources"].as_array().unwrap().len(), 1);

    // Configured but unavailable -> empty catalog, no error.
    let catalog = runtime.resource_catalog(Some("broken"), timeout).expect("unavailable catalog");
    assert_eq!(catalog["resources"], serde_json::json!([]));
    assert_eq!(catalog["templates"], serde_json::json!([]));

    // Unknown server -> Bun's MCP.NotFoundError message.
    let error = runtime.resource_catalog(Some("unknown"), timeout);
    assert_eq!(error.unwrap_err(), "MCP server not found: unknown");

    // Read happy path and tagging.
    let content = runtime
        .read_resource("fixture", "fixture://demo", timeout)
        .expect("read resource");
    assert_eq!(content["server"], "fixture");
    assert_eq!(content["uri"], "fixture://demo");
    assert_eq!(content["contents"][0]["text"], "fixture body");

    // Configured but unavailable -> "not connected" ToolFailure message.
    let error = runtime.read_resource("broken", "fixture://demo", timeout);
    assert_eq!(
        error.unwrap_err(),
        "MCP server \"broken\" is not connected or does not expose resources"
    );

    // Unknown server -> NotFoundError message.
    let error = runtime.read_resource("unknown", "fixture://demo", timeout);
    assert_eq!(error.unwrap_err(), "MCP server not found: unknown");
}

#[test]
fn runtime_unadvertised_resources_are_empty() {
    let mut servers = BTreeMap::new();
    let mut environment = BTreeMap::new();
    environment.insert("FIXTURE_NO_RESOURCES".to_string(), "1".to_string());
    servers.insert(
        "fixture".to_string(),
        ServerConfig::Local(devcode_mcp::LocalConfig {
            command: vec![fixture_path()],
            cwd: None,
            environment,
            disabled: false,
        }),
    );
    let timeout = Duration::from_secs(5);
    let db = std::path::Path::new(":memory:");
    let mut runtime = devcode_mcp::Runtime::connect_all(&servers, timeout, db);

    let catalog = runtime.resource_catalog(None, timeout).expect("catalog");
    assert_eq!(catalog["resources"], serde_json::json!([]));
    assert_eq!(catalog["templates"], serde_json::json!([]));

    let catalog = runtime.resource_catalog(Some("fixture"), timeout).expect("filtered catalog");
    assert_eq!(catalog["resources"], serde_json::json!([]));

    let error = runtime.read_resource("fixture", "fixture://demo", timeout);
    assert_eq!(
        error.unwrap_err(),
        "MCP server \"fixture\" is not connected or does not expose resources"
    );
}

#[test]
fn tool_name_namespacing() {
    assert_eq!(namespaced_tool_name("figma", "whoami"), "figma_whoami");
    assert_eq!(namespaced_tool_name("my.server", "a b"), "my_server_a_b");
}

#[test]
fn http_handshake_and_call() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let handle = std::thread::spawn(move || {
        let server = tiny_http::Server::from_listener(listener, None).unwrap();
        for mut request in server.incoming_requests() {
            let mut body = String::new();
            request.as_reader().read_to_string(&mut body).unwrap();
            let message: serde_json::Value = serde_json::from_str(&body).unwrap();
            if message.get("method").and_then(|m| m.as_str()) == Some("notifications/initialized") {
                let _ = request.respond(tiny_http::Response::empty(202));
                continue;
            }
            let id = message
                .get("id")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let method = message["method"].as_str().unwrap_or("");
            let result = match method {
                "initialize" => serde_json::json!({
                    "protocolVersion": "2025-03-26",
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "fixture", "version": "0.0.0" },
                }),
                "tools/list" => serde_json::json!({
                    "tools": [{ "name": "ping", "description": "ping", "inputSchema": { "type": "object" } }],
                }),
                "tools/call" => serde_json::json!({
                    "content": [{ "type": "text", "text": "pong" }],
                }),
                _ => serde_json::json!({}),
            };
            let response_body =
                serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string();
            let mut response = tiny_http::Response::from_string(response_body);
            response.add_header(
                tiny_http::Header::from_bytes(&b"content-type"[..], &b"application/json"[..])
                    .unwrap(),
            );
            response.add_header(
                tiny_http::Header::from_bytes(&b"mcp-session-id"[..], &b"session-1"[..]).unwrap(),
            );
            let _ = request.respond(response);
        }
    });
    // let the server thread start
    std::thread::sleep(Duration::from_millis(100));
    let config = ServerConfig::Remote(devcode_mcp::RemoteConfig {
        url: url.clone(),
        headers: BTreeMap::new(),
        needs_oauth: false,
        disabled: false,
    });
    let timeout = Duration::from_secs(5);
    let db = std::path::Path::new(":memory:");
    let mut connection = Connection::connect("remote", &config, timeout, db).expect("connect");
    let tools = connection.list_tools(timeout).expect("list tools");
    assert_eq!(tools[0].name, "ping");
    let result = connection
        .call_tool("ping", &serde_json::json!({}), timeout)
        .expect("call");
    assert_eq!(result, "pong");
    drop(connection);
    drop(handle);
}

#[test]
fn http_resources_listed_and_read() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let handle = std::thread::spawn(move || {
        let server = tiny_http::Server::from_listener(listener, None).unwrap();
        for mut request in server.incoming_requests() {
            let mut body = String::new();
            request.as_reader().read_to_string(&mut body).unwrap();
            let message: serde_json::Value = serde_json::from_str(&body).unwrap();
            if message.get("method").and_then(|m| m.as_str()) == Some("notifications/initialized") {
                let _ = request.respond(tiny_http::Response::empty(202));
                continue;
            }
            let id = message
                .get("id")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let method = message["method"].as_str().unwrap_or("");
            let result = match method {
                "initialize" => serde_json::json!({
                    "protocolVersion": "2025-03-26",
                    "capabilities": { "tools": {}, "resources": {} },
                    "serverInfo": { "name": "fixture", "version": "0.0.0" },
                }),
                "tools/list" => serde_json::json!({
                    "tools": [{ "name": "ping", "description": "ping", "inputSchema": { "type": "object" } }],
                }),
                "resources/list" => serde_json::json!({
                    "resources": [{ "uri": "http://fixture/demo", "name": "demo", "mimeType": "text/markdown" }],
                }),
                "resources/templates/list" => serde_json::json!({
                    "resourceTemplates": [{ "uriTemplate": "http://fixture/item/{id}", "name": "item" }],
                }),
                "resources/read" => serde_json::json!({
                    "contents": [{ "type": "text", "uri": "http://fixture/demo", "text": "body", "mimeType": "text/markdown" }],
                }),
                _ => serde_json::json!({}),
            };
            let response_body =
                serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string();
            let mut response = tiny_http::Response::from_string(response_body);
            response.add_header(
                tiny_http::Header::from_bytes(&b"content-type"[..], &b"application/json"[..])
                    .unwrap(),
            );
            response.add_header(
                tiny_http::Header::from_bytes(&b"mcp-session-id"[..], &b"session-1"[..]).unwrap(),
            );
            let _ = request.respond(response);
        }
    });
    std::thread::sleep(Duration::from_millis(100));
    let config = ServerConfig::Remote(devcode_mcp::RemoteConfig {
        url: url.clone(),
        headers: BTreeMap::new(),
        needs_oauth: false,
        disabled: false,
    });
    let timeout = Duration::from_secs(5);
    let db = std::path::Path::new(":memory:");
    let mut connection = Connection::connect("remote", &config, timeout, db).expect("connect");
    assert!(connection.resources_advertised());
    let resources = connection.list_resources(timeout).expect("resources/list");
    assert_eq!(resources.len(), 1);
    assert_eq!(resources[0]["uri"], "http://fixture/demo");
    let templates = connection
        .list_resource_templates(timeout)
        .expect("resources/templates/list");
    assert_eq!(templates[0]["uriTemplate"], "http://fixture/item/{id}");
    let contents = connection
        .read_resource("http://fixture/demo", timeout)
        .expect("resources/read");
    assert_eq!(contents[0]["text"], "body");
    drop(connection);
    drop(handle);
}
