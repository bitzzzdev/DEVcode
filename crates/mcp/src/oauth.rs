//! OAuth 2.0 client for MCP remote servers (RFC 9728 discovery, PKCE, DCR, loopback callback).

use std::collections::BTreeMap;
use std::fmt;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ring::digest;
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ureq::Agent;

#[derive(Debug)]
pub enum OAuthError {
    Discovery(String),
    Registration(String),
    Pkce(String),
    Callback(String),
    TokenExchange(String),
    Store(String),
    Config(String),
}

impl fmt::Display for OAuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OAuthError::Discovery(msg) => write!(f, "OAuth discovery failed: {msg}"),
            OAuthError::Registration(msg) => write!(f, "OAuth client registration failed: {msg}"),
            OAuthError::Pkce(msg) => write!(f, "OAuth PKCE failed: {msg}"),
            OAuthError::Callback(msg) => write!(f, "OAuth callback failed: {msg}"),
            OAuthError::TokenExchange(msg) => write!(f, "OAuth token exchange failed: {msg}"),
            OAuthError::Store(msg) => write!(f, "OAuth credential store failed: {msg}"),
            OAuthError::Config(msg) => write!(f, "OAuth configuration error: {msg}"),
        }
    }
}

impl std::error::Error for OAuthError {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtectedResourceMetadata {
    pub resource: String,
    #[serde(rename = "authorization_servers", default)]
    pub authorization_servers: Vec<String>,
    #[serde(rename = "scopes_supported", default)]
    pub scopes_supported: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AuthorizationServerMetadata {
    pub issuer: String,
    #[serde(rename = "authorization_endpoint", default)]
    pub authorization_endpoint: Option<String>,
    #[serde(rename = "token_endpoint", default)]
    pub token_endpoint: Option<String>,
    #[serde(rename = "registration_endpoint", default)]
    pub registration_endpoint: Option<String>,
    #[serde(rename = "scopes_supported", default)]
    pub scopes_supported: Vec<String>,
    #[serde(rename = "code_challenge_methods_supported", default)]
    pub code_challenge_methods_supported: Vec<String>,
    #[serde(rename = "token_endpoint_auth_methods_supported", default)]
    pub token_endpoint_auth_methods_supported: Vec<String>,
}

pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

pub fn pkce_s256() -> Result<Pkce, OAuthError> {
    let mut bytes = [0u8; 32];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| OAuthError::Pkce("random source unavailable".into()))?;
    let verifier = URL_SAFE_NO_PAD.encode(bytes);
    let hash = digest::digest(&digest::SHA256, verifier.as_bytes());
    let challenge = URL_SAFE_NO_PAD.encode(hash.as_ref());
    Ok(Pkce { verifier, challenge })
}

/// RFC 9728 discovery: protected-resource metadata at the resource's origin (with the
/// resource path appended to the well-known prefix), then the authorization server
/// document it names. Path-less origins fall back to the root well-known URI.
pub fn discover(agent: &Agent, server_url: &str) -> Result<(String, AuthorizationServerMetadata), OAuthError> {
    let resource: ProtectedResourceMetadata = fetch_json(agent, &well_known_candidates(server_url, "oauth-protected-resource"))
        .map_err(|error| OAuthError::Discovery(error))?;
    let auth_server_url = resource
        .authorization_servers
        .first()
        .ok_or_else(|| OAuthError::Discovery("protected resource lists no authorization servers".into()))?
        .trim_end_matches('/')
        .to_owned();
    let metadata: AuthorizationServerMetadata = fetch_json(agent, &well_known_candidates(&auth_server_url, "oauth-authorization-server"))
        .map_err(OAuthError::Discovery)?;
    Ok((auth_server_url, metadata))
}

/// `<origin>/.well-known/<suffix><path>` first, `<origin>/.well-known/<suffix>` as fallback (RFC 9728/8414).
fn well_known_candidates(base: &str, suffix: &str) -> Vec<String> {
    let base = base.trim_end_matches('/');
    let scheme_end = base.find("://").map(|index| index + 3).unwrap_or(0);
    let after = &base[scheme_end..];
    let path_start = after.find('/').map(|index| scheme_end + index).unwrap_or(base.len());
    let origin = &base[..path_start];
    let path = base[path_start..].trim_end_matches('/');
    let root = format!("{origin}/.well-known/{suffix}");
    if path.is_empty() {
        vec![root]
    } else {
        vec![format!("{root}{path}"), root]
    }
}

fn fetch_json<T: serde::de::DeserializeOwned>(agent: &Agent, candidates: &[String]) -> Result<T, String> {
    let mut last_error = String::new();
    for url in candidates {
        match agent.get(url).set("accept", "application/json").call() {
            Ok(response) => {
                return response
                    .into_json::<T>()
                    .map_err(|error| format!("{url} JSON: {error}"));
            }
            Err(error) => last_error = format!("{url}: {error}"),
        }
    }
    Err(last_error)
}

pub fn register_client(
    agent: &Agent,
    registration_url: &str,
    redirect_uris: &[String],
    scope: &str,
) -> Result<String, OAuthError> {
    let body = serde_json::json!({
        "client_name": "devcode",
        "redirect_uris": redirect_uris,
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "none",
        "scope": scope,
    });
    let response = agent
        .post(registration_url)
        .set("content-type", "application/json")
        .set("accept", "application/json")
        .send_json(body)
        .map_err(|error| OAuthError::Registration(format!("{registration_url}: {error}")))?;
    let value: Value = response
        .into_json()
        .map_err(|error| OAuthError::Registration(format!("registration JSON: {error}")))?;
    value
        .get("client_id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| OAuthError::Registration("registration response has no client_id".into()))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredOAuth {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(rename = "methodID")]
    pub method_id: String,
    pub refresh: String,
    pub access: String,
    pub expires: u64,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, Value>,
}

pub struct CredentialStore {
    db: Arc<Mutex<rusqlite::Connection>>,
}

impl CredentialStore {
    pub fn open(db_path: &str) -> Result<Self, OAuthError> {
        let connection = rusqlite::Connection::open(db_path)
            .map_err(|error| OAuthError::Store(format!("open {db_path}: {error}")))?;
        connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS credential (\
                 id text PRIMARY KEY, \
                 integration_id text, \
                 label text NOT NULL, \
                 value text NOT NULL, \
                 connector_id text, \
                 method_id text, \
                 active integer, \
                 time_created integer NOT NULL, \
                 time_updated integer NOT NULL);",
            )
            .map_err(|error| OAuthError::Store(format!("credential table: {error}")))?;
        Ok(Self { db: Arc::new(Mutex::new(connection)) })
    }

    pub fn integration_id(name: &str, url: &str) -> String {
        let key = format!("{name}\u{0}{url}");
        format!("mcp_{}", &sha1_hex(&key)[..16])
    }

    pub fn get(&self, integration_id: &str) -> Result<Option<StoredOAuth>, OAuthError> {
        let db = self.db.lock().map_err(|_| OAuthError::Store("store poisoned".into()))?;
        let row: Option<String> = db
            .query_row(
                "SELECT value FROM credential WHERE integration_id = ?1 ORDER BY active DESC, time_created DESC LIMIT 1",
                [integration_id],
                |row| row.get::<_, String>(0),
            )
            .ok();
        drop(db);
        let Some(json) = row else { return Ok(None) };
        serde_json::from_str(&json).map(Some).map_err(|error| OAuthError::Store(format!("credential JSON: {error}")))
    }

    pub fn save(&self, integration_id: &str, label: &str, value: &StoredOAuth) -> Result<(), OAuthError> {
        let json = serde_json::to_string(value).map_err(|error| OAuthError::Store(format!("serialize: {error}")))?;
        // Bun uses cred_<ascending>; two saves in the same millisecond must not collide on the primary key.
        let mut random = [0u8; 8];
        SystemRandom::new()
            .fill(&mut random)
            .map_err(|_| OAuthError::Store("random source unavailable".into()))?;
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
        let id = format!("cred_{now}{:016x}", u64::from_le_bytes(random));
        let mut db = self.db.lock().map_err(|_| OAuthError::Store("store poisoned".into()))?;
        let tx = db
            .transaction()
            .map_err(|error| OAuthError::Store(format!("transaction: {error}")))?;
        tx.execute("UPDATE credential SET active = 0 WHERE integration_id = ?1", [integration_id])
            .map_err(|error| OAuthError::Store(format!("deactivate: {error}")))?;
        tx.execute(
            "INSERT INTO credential (id, integration_id, label, value, active, time_created, time_updated) \
             VALUES (?1, ?2, ?3, ?4, 1, ?5, ?5)",
            (id, integration_id, label, json, now as i64),
        )
        .map_err(|error| OAuthError::Store(format!("insert: {error}")))?;
        tx.commit().map_err(|error| OAuthError::Store(format!("commit: {error}")))?;
        Ok(())
    }
}

fn sha1_hex(input: &str) -> String {
    use sha1::{Digest, Sha1};
    Sha1::digest(input.as_bytes())
        .as_slice()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn url_encode(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    for byte in input.bytes() {
        if byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_' || byte == b'.' {
            output.push(byte as char);
        } else {
            output.push('%');
            output.push_str(&format!("{byte:02X}"));
        }
    }
    output
}

pub fn login(
    agent: &Agent,
    name: &str,
    url: &str,
    scope: Option<&str>,
    callback_port: u16,
) -> Result<StoredOAuth, OAuthError> {
    let (auth_server, metadata) = discover(agent, url)?;
    let token_endpoint = metadata
        .token_endpoint
        .ok_or_else(|| OAuthError::Discovery("no token_endpoint".into()))?;
    let authorization_endpoint = metadata
        .authorization_endpoint
        .ok_or_else(|| OAuthError::Discovery("no authorization_endpoint".into()))?;

    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, callback_port))
        .map_err(|error| OAuthError::Callback(format!("bind 127.0.0.1:{callback_port}: {error}")))?;
    let port = listener
        .local_addr()
        .map_err(|error| OAuthError::Callback(format!("local addr: {error}")))?
        .port();
    let redirect = format!("http://127.0.0.1:{port}/callback");

    let pkce = pkce_s256()?;
    let state = URL_SAFE_NO_PAD.encode({
        let mut bytes = [0u8; 32];
        SystemRandom::new()
            .fill(&mut bytes)
            .map_err(|_| OAuthError::Pkce("random state".into()))?;
        bytes
    });

    let client_id = register_client(
        agent,
        metadata
            .registration_endpoint
            .as_deref()
            .ok_or_else(|| OAuthError::Registration("no registration_endpoint".into()))?,
        &[redirect.clone()],
        scope.unwrap_or(""),
    )?;

    let auth_url = format!(
        "{authorization_endpoint}?response_type=code&client_id={}&redirect_uri={}&scope={}&state={}&code_challenge={}&code_challenge_method=S256",
        url_encode(&client_id),
        url_encode(&redirect),
        url_encode(scope.unwrap_or("")),
        url_encode(&state),
        url_encode(&pkce.challenge),
    );

    println!("Authorize DEVcode with MCP server \"{name}\":");
    println!("  {auth_url}");
    println!("  (loopback callback at {redirect})");

    let (code, _code_verifier) = wait_for_callback(listener, &state)?;

    let params = vec![
        ("grant_type", "authorization_code"),
        ("code", &code),
        ("redirect_uri", &redirect),
        ("client_id", &client_id),
        ("code_verifier", &pkce.verifier),
    ];
    let response = agent
        .post(&token_endpoint)
        .set("content-type", "application/x-www-form-urlencoded")
        .set("accept", "application/json")
        .send_form(&params)
        .map_err(|error| OAuthError::TokenExchange(format!("{token_endpoint}: {error}")))?;
    let tokens: Value = response
        .into_json()
        .map_err(|error| OAuthError::TokenExchange(format!("token JSON: {error}")))?;
    let access = tokens
        .get("access_token")
        .and_then(Value::as_str)
        .ok_or_else(|| OAuthError::TokenExchange("no access_token".into()))?
        .to_owned();
    let refresh = tokens
        .get("refresh_token")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let expires_in: u64 = tokens.get("expires_in").and_then(Value::as_u64).unwrap_or(0);
    let expires = if expires_in == 0 {
        0
    } else {
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
            + expires_in * 1000
    };
    let mut metadata_map = BTreeMap::new();
    metadata_map.insert("serverUrl".into(), Value::String(url.to_owned()));
    if let Some(token_type) = tokens.get("token_type").and_then(Value::as_str) {
        metadata_map.insert("tokenType".into(), Value::String(token_type.to_owned()));
    }
    metadata_map.insert("issuer".into(), Value::String(auth_server.clone()));
    metadata_map.insert("client".into(), serde_json::json!({ "client_id": client_id }));

    Ok(StoredOAuth {
        kind: "oauth".into(),
        method_id: CredentialStore::integration_id(name, url),
        access,
        refresh,
        expires,
        metadata: metadata_map,
    })
}

pub fn refresh(
    agent: &Agent,
    _name: &str,
    url: &str,
    stored: &StoredOAuth,
) -> Result<StoredOAuth, OAuthError> {
    let (_auth_server, metadata) = discover(agent, url)?;
    let token_endpoint = metadata
        .token_endpoint
        .ok_or_else(|| OAuthError::Discovery("no token_endpoint".into()))?;

    let client_id = stored
        .metadata
        .get("client")
        .and_then(|client| client.get("client_id"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();

    let params = [
        ("grant_type", "refresh_token"),
        ("refresh_token", &stored.refresh),
        ("client_id", &client_id),
    ];
    let response = agent
        .post(&token_endpoint)
        .set("content-type", "application/x-www-form-urlencoded")
        .set("accept", "application/json")
        .send_form(&params)
        .map_err(|error| OAuthError::TokenExchange(format!("{token_endpoint}: {error}")))?;
    let tokens: Value = response
        .into_json()
        .map_err(|error| OAuthError::TokenExchange(format!("token JSON: {error}")))?;
    let access = tokens
        .get("access_token")
        .and_then(Value::as_str)
        .ok_or_else(|| OAuthError::TokenExchange("no access_token".into()))?
        .to_owned();
    let refresh = tokens
        .get("refresh_token")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let expires_in: u64 = tokens.get("expires_in").and_then(Value::as_u64).unwrap_or(0);
    let expires = if expires_in == 0 {
        0
    } else {
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
            + expires_in * 1000
    };
    let mut metadata = stored.metadata.clone();
    if let Some(token_type) = tokens.get("token_type").and_then(Value::as_str) {
        metadata.insert("tokenType".into(), Value::String(token_type.to_owned()));
    }
    metadata.insert("issuer".into(), Value::String(_auth_server.clone()));

    Ok(StoredOAuth {
        kind: "oauth".into(),
        method_id: stored.method_id.clone(),
        access,
        refresh,
        expires,
        metadata,
    })
}

fn wait_for_callback(listener: TcpListener, expected_state: &str) -> Result<(String, String), OAuthError> {
    let mut stream = listener
        .incoming()
        .next()
        .ok_or_else(|| OAuthError::Callback("callback listener closed".into()))?
        .map_err(|error| OAuthError::Callback(format!("accept: {error}")))?;
    let mut buffer = [0u8; 8192];
    let read = stream
        .read(&mut buffer)
        .map_err(|error| OAuthError::Callback(format!("read: {error}")))?;
    let request = String::from_utf8_lossy(&buffer[..read]);
    let path = request.lines().next().unwrap_or_default().to_string();
    let Some(query) = path.split('?').nth(1) else {
        return Err(OAuthError::Callback("callback path without query".into()));
    };
    let mut params = BTreeMap::new();
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let Some((key, value)) = pair.split_once('=') else { continue };
        params.insert(key.to_owned(), value.to_owned());
    }
    let returned_state = params.get("state").cloned().unwrap_or_default();
    let code = params.get("code").cloned();
    let error_text = params
        .get("error")
        .or_else(|| params.get("error_description"))
        .cloned();
    let mut respond = |status: &str, body: &str| -> Result<(), OAuthError> {
        write!(
            stream,
            "HTTP/1.1 {status}\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
        .map_err(|error| OAuthError::Callback(format!("write response: {error}")))?;
        stream.flush().map_err(|error| OAuthError::Callback(format!("flush response: {error}")))
    };
    if let Some(error_text) = error_text {
        respond("400 Bad Request", &format!("<html><body>Authorization failed: {error_text}</body></html>"))?;
        return Err(OAuthError::Callback(format!("authorization server returned: {error_text}")));
    }
    if returned_state != expected_state {
        respond("400 Bad Request", "<html><body>OAuth state mismatch</body></html>")?;
        return Err(OAuthError::Callback("state mismatch".into()));
    }
    let Some(code) = code else {
        respond("400 Bad Request", "<html><body>Missing authorization code</body></html>")?;
        return Err(OAuthError::Callback("missing authorization code".into()));
    };
    respond("200 OK", "<html><body>Authorization complete.</body></html>")?;
    Ok((code, String::new()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn pkce_challenge_is_sha256_of_verifier() {
        let pkce = pkce_s256().expect("pkce");
        let hash = digest::digest(&digest::SHA256, pkce.verifier.as_bytes());
        assert_eq!(URL_SAFE_NO_PAD.encode(hash.as_ref()), pkce.challenge);
        assert_ne!(pkce.verifier, pkce.challenge);
    }

    #[test]
    fn integration_id_matches_bun_derivation() {
        use sha1::{Digest, Sha1};
        let id = CredentialStore::integration_id("figma", "https://mcp.figma.com/mcp");
        let hex = Sha1::digest(b"figma\0https://mcp.figma.com/mcp")
            .as_slice()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        assert_eq!(id, format!("mcp_{}", &hex[..16]));
    }

    #[test]
    fn credential_store_roundtrip() {
        let store = CredentialStore::open(":memory:").expect("open");
        let id = "mcp_0123456789abcdef";
        assert!(store.get(id).expect("get").is_none());
        let stored = StoredOAuth {
            kind: "oauth".into(),
            method_id: id.into(),
            refresh: "refresh-token".into(),
            access: "access-token".into(),
            expires: 4_000_000_000_000,
            metadata: BTreeMap::from([("serverUrl".to_owned(), Value::String("https://x".into()))]),
        };
        store.save(id, "default", &stored).expect("save");
        let loaded = store.get(id).expect("get").expect("row");
        assert_eq!(loaded.access, "access-token");
        assert_eq!(loaded.refresh, "refresh-token");
        assert_eq!(loaded.method_id, id);
        // Bun-compatible JSON field names
        let json = serde_json::to_value(&loaded).unwrap();
        assert_eq!(json["type"], "oauth");
        assert_eq!(json["methodID"], id);
        // saving again deactivates the older row; the fresh one is returned first
        let updated = StoredOAuth { access: "access-token-2".into(), ..loaded };
        store.save(id, "default", &updated).expect("save2");
        assert_eq!(store.get(id).expect("get2").unwrap().access, "access-token-2");
    }

    // Live network smoke: `cargo test -p devcode-mcp -- --ignored discover_figma`
    #[test]
    #[ignore]
    fn discover_figma_live() {
        let agent = Agent::new();
        let (auth_server, metadata) = discover(&agent, "https://mcp.figma.com/mcp").expect("discovery");
        assert_eq!(auth_server, "https://api.figma.com");
        assert_eq!(metadata.token_endpoint.as_deref(), Some("https://api.figma.com/v1/oauth/token"));
        assert_eq!(metadata.registration_endpoint.as_deref(), Some("https://api.figma.com/v1/oauth/mcp/register"));
        assert!(metadata.code_challenge_methods_supported.contains(&"S256".to_string()));
    }

    #[test]
    fn refresh_against_loopback_authorization_server() {
        use tiny_http::{Header, Response, Server};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let expected_refresh = "refresh-token-1";
        let server_base = base.clone();
        let handle = std::thread::spawn(move || {
            let server = Server::from_listener(listener, None).unwrap();
            for mut request in server.incoming_requests() {
                let url = request.url().to_string();
                let body = match url.as_str() {
                    "/.well-known/oauth-protected-resource" => {
                        serde_json::json!({
                            "resource": server_base,
                            "authorization_servers": [server_base],
                        })
                        .to_string()
                    }
                    "/.well-known/oauth-authorization-server" => {
                        serde_json::json!({
                            "issuer": server_base,
                            "authorization_endpoint": format!("{server_base}/authorize"),
                            "token_endpoint": format!("{server_base}/token"),
                            "registration_endpoint": format!("{server_base}/register"),
                        })
                        .to_string()
                    }
                    "/token" => {
                        let mut body = String::new();
                        std::io::Read::read_to_string(&mut request.as_reader(), &mut body).unwrap();
                        assert!(body.contains(&format!("refresh_token={expected_refresh}")), "body: {body}");
                        assert!(body.contains("grant_type=refresh_token"));
                        assert!(body.contains("client_id=figma-client"));
                        serde_json::json!({
                            "access_token": "fresh-access",
                            "refresh_token": "refresh-token-2",
                            "expires_in": 3600,
                            "token_type": "Bearer",
                        })
                        .to_string()
                    }
                    other => panic!("unexpected path {other}"),
                };
                let mut response = Response::from_string(body);
                response.add_header(Header::from_bytes(&b"content-type"[..], &b"application/json"[..]).unwrap());
                request.respond(response).unwrap();
            }
        });
        std::thread::sleep(Duration::from_millis(50));
        let stored = StoredOAuth {
            kind: "oauth".into(),
            method_id: CredentialStore::integration_id("figma", &base),
            refresh: expected_refresh.into(),
            access: "stale-access".into(),
            expires: 0,
            metadata: BTreeMap::from([(
                "client".to_owned(),
                serde_json::json!({ "client_id": "figma-client" }),
            )]),
        };
        let agent = Agent::new();
        let refreshed = refresh(&agent, "figma", &base, &stored).expect("refresh");
        assert_eq!(refreshed.access, "fresh-access");
        assert_eq!(refreshed.refresh, "refresh-token-2");
        assert!(refreshed.expires > 0);
        drop(handle);
    }
}
