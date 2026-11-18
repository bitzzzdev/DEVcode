//! Service discovery layer: managed-service config, registration file, health probe,
//! default ports. File formats and health semantics mirror the TypeScript build so the
//! Rust and Bun clients and servers interoperate on the same machine.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

pub const DEFAULT_HOSTNAME: &str = "127.0.0.1";
pub const AUTH_USERNAME: &str = "devcode";

pub fn default_port(channel: &str) -> u16 {
    channel_port(channel)
}

pub fn default_port_for(channel: &str, windows: bool) -> u16 {
    let port = channel_port(channel);
    // Windows reserves parts of 0xC000-0xFFFF (ms-inmines WinNAT / WSL); drop into 0x7000 like the TS build.
    if windows && port >= 0x8000 {
        return 0x7000 | (port & 0x0fff);
    }
    port
}

fn channel_port(channel: &str) -> u16 {
    match channel {
        // The Bun build keeps 0xc0df (49375); the rust build starts at 0xc0e0 (49376) so both run side by side.
        "latest" | "dev" | "beta" | "next" => 0xc0e0,
        "local" => 0xc0e1,
        _ => 10_000 + (fast_hash(channel) % 50_000) as u16,
    }
}

fn fast_hash(input: &str) -> u64 {
    // FNV-1a; only used to spread non-release channel names, matching upstream intent.
    let mut hash = 0xcbf29ce484222325u64;
    for byte in input.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

pub fn service_url(hostname: &str, port: u16) -> String {
    if hostname.contains(':') {
        format!("http://[{hostname}]:{port}")
    } else {
        format!("http://{hostname}:{port}")
    }
}

pub fn config_file(paths: &devcode_util::Paths, channel: &str) -> PathBuf {
    paths.config.join(config_filename(channel))
}

pub fn state_file(paths: &devcode_util::Paths, filename: &str) -> PathBuf {
    paths.state.join(filename)
}

pub fn config_filename(channel: &str) -> String {
    match channel {
        "latest" | "dev" | "beta" | "next" => "service.json".to_owned(),
        other => format!(
            "service-{}.json",
            other
                .chars()
                .map(
                    |c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                        c
                    } else {
                        '-'
                    }
                )
                .collect::<String>()
        ),
    }
}

/// Managed-service config. Unknown keys round-trip untouched so builds with newer
/// fields written by the Bun CLI are not corrupted.
#[derive(Debug, Default, Clone)]
pub struct Config {
    value: Map<String, Value>,
}

impl Config {
    pub fn read(path: &Path) -> Config {
        let value = fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str::<Map<String, Value>>(&text).ok())
            .unwrap_or_default();
        Config { value }
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.value.get(key)
    }

    pub fn get_bool(&self, key: &str) -> Option<bool> {
        self.value.get(key).and_then(Value::as_bool)
    }

    pub fn get_port(&self) -> Option<u16> {
        self.value
            .get("port")
            .and_then(Value::as_u64)
            .and_then(|p| u16::try_from(p).ok())
    }

    pub fn get_password(&self) -> Option<String> {
        self.value
            .get("password")
            .and_then(Value::as_str)
            .map(str::to_owned)
    }

    pub fn set(&mut self, key: &str, value: Value) {
        self.value.insert(key.to_owned(), value);
    }

    pub fn unset(&mut self, key: &str) -> bool {
        self.value.remove(key).is_some()
    }

    pub fn write(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temp = path.with_extension(format!("tmp.{}", std::process::id()));
        fs::write(&temp, serde_json::to_string_pretty(&self.value)? + "\n")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&temp, fs::Permissions::from_mode(0o600))?;
        }
        fs::rename(&temp, path)
    }
}

/// Registration written by a running service; this is the discovery contract clients read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registration {
    pub id: String,
    pub version: String,
    pub url: String,
    pub pid: i64,
    pub password: String,
}

impl Registration {
    pub fn read(path: &Path) -> Option<Registration> {
        let value: Value = serde_json::from_str(&fs::read_to_string(path).ok()?).ok()?;
        Some(Registration {
            id: value.get("id")?.as_str()?.to_owned(),
            version: value.get("version")?.as_str()?.to_owned(),
            url: value.get("url")?.as_str()?.to_owned(),
            pid: value.get("pid")?.as_i64()?,
            password: value.get("password")?.as_str()?.to_owned(),
        })
    }

    pub fn write(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let body = json!({
            "id": self.id,
            "version": self.version,
            "url": self.url,
            "pid": self.pid,
            "password": self.password,
        });
        let temp = path.with_extension(format!("tmp.{}", self.id));
        fs::write(&temp, serde_json::to_string(&body)? + "\n")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&temp, fs::Permissions::from_mode(0o600))?;
        }
        fs::rename(&temp, path)
    }

    pub fn remove_if_owned(&self, path: &Path) {
        if Registration::read(path).as_ref() == Some(self) {
            let _ = fs::remove_file(path);
        }
    }
}

/// Generate a URL-safe 32-byte password, same shape as the TS build's randomBytes(32) base64url.
pub fn generate_password() -> String {
    use base64::Engine;
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).expect("system RNG unavailable");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

pub fn generate_id() -> String {
    use base64::Engine;
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes).expect("system RNG unavailable");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    Ready,
    Waiting,
    Failed,
    Incompatible,
    Unreachable,
}

/// Probe a registration's `/api/info` and classify it, mirroring packages/client/src/service-probe.ts.
pub fn probe(registration: &Registration, timeout_ms: u64) -> Health {
    let url = format!("{}/api/info", registration.url.trim_end_matches('/'));
    let credentials = format!("{AUTH_USERNAME}:{}", registration.password);
    let result = ureq::get(&url)
        .set(
            "authorization",
            &format!(
                "Basic {}",
                base64::Engine::encode(
                    &base64::engine::general_purpose::STANDARD,
                    credentials.as_bytes()
                )
            ),
        )
        .timeout(std::time::Duration::from_millis(timeout_ms))
        .call();
    match result {
        Err(ureq::Error::Status(404, _)) => Health::Incompatible,
        Err(ureq::Error::Status(500, _)) => Health::Failed,
        Ok(response) => {
            if !(200..300).contains(&response.status()) {
                return Health::Waiting;
            }
            let body: Value = match response.into_json() {
                Ok(body) => body,
                Err(_) => return Health::Unreachable,
            };
            let pid = body.get("pid").and_then(Value::as_i64);
            let version = body.get("version").and_then(Value::as_str);
            match (pid, version) {
                (Some(pid), Some(version))
                    if pid == registration.pid && version == registration.version =>
                {
                    Health::Ready
                }
                _ => Health::Unreachable,
            }
        }
        Err(_) => Health::Unreachable,
    }
}

/// Constant-time comparison so probing with the wrong password takes the same time as the right one.
pub fn constant_time_eq(left: &str, right: &str) -> bool {
    let (a, b) = (left.as_bytes(), right.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (l, r)| acc | (l ^ r)) == 0
}

#[allow(dead_code)]
fn _unused(_: BTreeMap<String, Value>) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_ports_match_typescript_build() {
        assert_eq!(default_port("latest"), 0xc0e0);
        assert_eq!(default_port("dev"), 0xc0e0);
        assert_eq!(default_port("beta"), 0xc0e0);
        assert_eq!(default_port("next"), 0xc0e0);
        assert_eq!(default_port("local"), 0xc0e1);
        assert_eq!(default_port_for("latest", true), 0x70e0);
        assert_eq!(default_port_for("dev", true), 0x70e0);
        assert_eq!(default_port_for("local", true), 0x70e1);
        assert_eq!(default_port_for("latest", false), 0xc0e0);
    }

    #[test]
    fn config_round_trips_unknown_keys() {
        let dir = std::env::temp_dir().join(format!("devcode-service-test-{}", generate_id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("service.json");
        let mut config = Config::default();
        config.set("port", json!(49375));
        config.set("futureField", json!({"nested": true}));
        config.write(&path).unwrap();

        let reread = Config::read(&path);
        assert_eq!(reread.get_port(), Some(49375));
        assert_eq!(reread.get("futureField"), Some(&json!({"nested": true})));

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn registration_round_trips_with_0600() {
        let dir = std::env::temp_dir().join(format!("devcode-registration-test-{}", generate_id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("service.json");
        let registration = Registration {
            id: "id-123".into(),
            version: "0.1.0-rust".into(),
            url: "http://127.0.0.1:49375".into(),
            pid: 12345,
            password: "secret".into(),
        };
        registration.write(&path).unwrap();
        assert_eq!(Registration::read(&path), Some(registration.clone()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        registration.remove_if_owned(&path);
        assert!(!path.exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn constant_time_eq_compares_bytes() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "abcd"));
    }
}
