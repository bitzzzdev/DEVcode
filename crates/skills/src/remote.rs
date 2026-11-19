//! Remote skill sources (Bun: `packages/core/src/skill/discovery.ts`).
//!
//! A `skills` config entry that parses as an `http(s)` URL is pulled into a
//! disk cache under `<cache>/skills/<sha1(base)>/<name>/` and the cached
//! roots are scanned as local skill directories. Fetch failures are fatal
//! to nothing: the entry is skipped and local skills still load (the Bun
//! build logs and continues the same way).
//!
//! Protocol (`<base>/index.json`):
//! `{ "skills": [{ "name": "deploy", "version": "1", "files": ["SKILL.md", ...] }] }`
//!
//! Files already on disk are never re-downloaded; when the index carries an
//! unmatched `version` the whole skill is refreshed atomically (staging
//! directory + rename swap; the old copy is restored on failure).

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use sha1::{Digest, Sha1};

const INDEX_TIMEOUT: Duration = Duration::from_secs(10);
const FILE_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
/// In-process reuse of a pulled source, mirroring the Bun build's cache
/// (its cache key is the base URL). `load` runs per agent step, so without
/// this every step would re-fetch `index.json`.
const CACHE_TTL: Duration = Duration::from_secs(60);

/// Whether `value` is a safe single path segment (Bun's `isSafeSegment`).
pub fn is_safe_segment(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && !value.contains('/')
        && !value.contains('\\')
        && !value.contains('\0')
}

/// Whether `value` is a safe relative file path (Bun's `isSafeRelativePath`).
/// Segments are percent-decoded before validation so `%2e%2e%2f` cannot
/// smuggle a traversal past the literal `..` check. Anything that parses as
/// a URL, carries a drive prefix, or holds `?`/`#` is rejected outright.
pub fn is_safe_relative_path(value: &str) -> bool {
    if value.is_empty()
        || value.contains('\\')
        || value.contains('\0')
        || value.contains('?')
        || value.contains('#')
    {
        return false;
    }
    if url::Url::parse(value).is_ok() {
        return false;
    }
    if value.starts_with('/') {
        return false;
    }
    let bytes = value.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' {
        return false; // drive-letter prefix, stricter than win32.isAbsolute
    }
    value.split('/').all(|segment| match percent_decode(segment) {
        Some(decoded) => is_safe_segment(&decoded),
        None => false,
    })
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn percent_decode(segment: &str) -> Option<String> {
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' => {
                let hi = *bytes.get(index + 1)?;
                let lo = *bytes.get(index + 2)?;
                out.push(hex(hi)? * 16 + hex(lo)?);
                index += 3;
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

fn percent_encode_segment(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

pub(crate) fn sha1_hex(input: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(input.as_bytes());
    format!("{:x}", hasher.finalize())
}

#[derive(serde::Deserialize)]
struct IndexSkill {
    name: String,
    version: Option<String>,
    files: Vec<String>,
}

#[derive(serde::Deserialize)]
struct Index {
    skills: Vec<IndexSkill>,
}

fn http_get(url: &str, timeout: Duration, accept_json: bool) -> Result<Vec<u8>, String> {
    let request = ureq::get(url).set("accept", if accept_json { "application/json" } else { "*/*" });
    let response = request
        .timeout(timeout)
        .call()
        .map_err(|error| format!("GET {url}: {error}"))?;
    if !(200..300).contains(&response.status()) {
        return Err(format!("GET {url}: status {}", response.status()));
    }
    let mut body = Vec::new();
    response
        .into_reader()
        .take(MAX_FILE_BYTES)
        .read_to_end(&mut body)
        .map_err(|error| format!("GET {url}: {error}"))?;
    Ok(body)
}

/// Download `url` to `destination`, creating parent directories. Existing
/// files are not re-downloaded (Bun's `download` semantics).
fn download(url: &str, destination: &Path) -> Result<(), String> {
    if destination.exists() {
        return Ok(());
    }
    let body = http_get(url, FILE_TIMEOUT, false)?;
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent).map_err(|error| format!("mkdir {}: {error}", parent.display()))?;
    }
    std::fs::write(destination, body).map_err(|error| format!("write {}: {error}", destination.display()))
}

/// Pull one remote skill source. Returns the cached skill roots that hold a
/// usable SKILL.md (or `<name>.md`); the caller scans them like local
/// directories. Errors are the caller's to log; nothing is downloaded when
/// the index cannot be fetched or parsed.
pub fn pull(cache_dir: &Path, url: &str) -> Result<Vec<PathBuf>, String> {
    let base = if url.ends_with('/') { url.to_string() } else { format!("{url}/") };
    let source = url::Url::parse(&base).map_err(|error| format!("invalid skill url {url}: {error}"))?;
    if !matches!(source.scheme(), "http" | "https") {
        return Err(format!("skill source must be http(s): {url}"));
    }
    let index_url = source.join("index.json").map_err(|error| format!("invalid skill url {url}: {error}"))?;
    let body = http_get(index_url.as_str(), INDEX_TIMEOUT, true)?;
    let index: Index = serde_json::from_slice(&body)
        .map_err(|error| format!("GET {index_url}: invalid index JSON: {error}"))?;
    let cache_root = cache_dir.join("skills").join(sha1_hex(&base));
    let mut roots = Vec::new();
    for skill in index.skills {
        match pull_skill(&source, &base, &cache_root, &skill) {
            Some(root) => roots.push(root),
            None => continue,
        }
    }
    Ok(roots)
}

fn pull_skill(source: &url::Url, base: &str, cache_root: &Path, skill: &IndexSkill) -> Option<PathBuf> {
    if !is_safe_segment(&skill.name) {
        eprintln!("skipping remote skill with unsafe name {:?}", skill.name);
        return None;
    }
    let markdown_name = format!("{}.md", skill.name);
    if !skill.files.iter().any(|file| file == "SKILL.md" || file == &markdown_name) {
        return None;
    }
    let root = cache_root.join(&skill.name);
    if root == cache_root || !root.starts_with(cache_root) {
        return None;
    }
    // Validate every file BEFORE downloading anything: one bad entry
    // invalidates the whole skill (Bun's `files.every(...)` gate).
    let skill_url = format!("{}{}/", base, percent_encode_segment(&skill.name));
    let files: Vec<(String, PathBuf)> = skill
        .files
        .iter()
        .map(|file| {
            if !is_safe_relative_path(file) {
                return None;
            }
            let resource = format!("{skill_url}{file}");
            let parsed = url::Url::parse(&resource).ok()?;
            if parsed.origin() != source.origin() {
                return None;
            }
            Some((resource, root.join(file)))
        })
        .collect::<Option<_>>()?;
    match &skill.version {
        None => refresh_files(&files),
        Some(version) => {
            let current = std::fs::read_to_string(root.join(".devcode-version"))
                .map(|text| text.trim().to_string())
                .ok();
            if current.as_deref() == Some(version.as_str()) {
                // Same version: fetch only files that went missing.
                refresh_files(&files);
            } else if !refresh_versioned(&root, &markdown_name, &files, version) {
                eprintln!("failed to refresh remote skill {:?}", skill.name);
            }
        }
    }
    if root.join("SKILL.md").exists() || root.join(&markdown_name).exists() {
        return Some(root);
    }
    None
}

/// Download whatever is missing; failures only mean the caller may end up
/// with an incomplete (or absent) skill directory.
fn refresh_files(files: &[(String, PathBuf)]) {
    for (resource, destination) in files {
        if let Err(error) = download(resource, destination) {
            eprintln!("{error}");
        }
    }
}

/// Version bump: stage everything next to the skill, swap atomically.
/// On any failure the previously cached skill is left untouched.
fn refresh_versioned(root: &Path, markdown_name: &str, files: &[(String, PathBuf)], version: &str) -> bool {
    let token = format!("{}-{}", std::process::id(), std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0));
    let staging = PathBuf::from(format!("{}.tmp-{token}", root.display()));
    let backup = PathBuf::from(format!("{}.old-{token}", root.display()));
    let staged = (|| -> Option<()> {
        for (resource, destination) in files {
            let relative = destination.strip_prefix(root).ok()?;
            download(resource, &staging.join(relative)).ok()?;
        }
        if !staging.join("SKILL.md").exists() && !staging.join(markdown_name).exists() {
            return None;
        }
        Some(())
    })();
    if staged.is_none() {
        let _ = std::fs::remove_dir_all(&staging);
        return false;
    }
    if let Err(error) = std::fs::write(staging.join(".devcode-version"), version) {
        eprintln!("failed to write version marker: {error}");
        let _ = std::fs::remove_dir_all(&staging);
        return false;
    }
    let cached = root.exists();
    if cached {
        if let Err(error) = std::fs::rename(root, &backup) {
            eprintln!("failed to back up cached skill: {error}");
            let _ = std::fs::remove_dir_all(&staging);
            return false;
        }
    }
    match std::fs::rename(&staging, root) {
        Ok(()) => {
            if cached {
                let _ = std::fs::remove_dir_all(&backup);
            }
            true
        }
        Err(error) => {
            eprintln!("failed to swap refreshed skill into place: {error}");
            if cached {
                let _ = std::fs::rename(&backup, root);
            }
            false
        }
    }
}

struct CacheEntry {
    at: Instant,
    roots: Vec<PathBuf>,
}

static PULL_CACHE: Mutex<Option<HashMap<String, CacheEntry>>> = Mutex::new(None);

/// Cached wrapper used by `load` (one HTTP round trip per source per TTL,
/// not per agent step). A failed pull reuses the stale cache when it exists —
/// remote availability should not flap the skill set mid-session.
pub fn pull_cached(cache_dir: &Path, url: &str) -> Vec<PathBuf> {
    {
        let Ok(mut guard) = PULL_CACHE.lock() else {
            return pull(cache_dir, url).unwrap_or_default();
        };
        let cache = guard.get_or_insert_with(HashMap::new);
        if let Some(entry) = cache.get(url) {
            if entry.at.elapsed() < CACHE_TTL {
                return entry.roots.clone();
            }
        }
    }
    match pull(cache_dir, url) {
        Ok(roots) => {
            if let Ok(mut guard) = PULL_CACHE.lock() {
                guard
                    .get_or_insert_with(HashMap::new)
                    .insert(url.to_string(), CacheEntry { at: Instant::now(), roots: roots.clone() });
            }
            roots
        }
        Err(error) => {
            eprintln!("failed to pull remote skills from {url}: {error}");
            PULL_CACHE
                .lock()
                .ok()
                .and_then(|guard| guard.as_ref().and_then(|cache| cache.get(url).map(|entry| entry.roots.clone())))
                .unwrap_or_default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("devcode-remote-skills-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn safe_relative_path_rules() {
        assert!(is_safe_relative_path("SKILL.md"));
        assert!(is_safe_relative_path("scripts/run.sh"));
        assert!(is_safe_relative_path("a%20b/file.txt"));
        assert!(!is_safe_relative_path("../escape.txt"));
        assert!(!is_safe_relative_path("a/../b"));
        assert!(!is_safe_relative_path("%2e%2e%2fescape.txt"));
        assert!(!is_safe_relative_path("/etc/passwd"));
        assert!(!is_safe_relative_path("c:/temp/x.txt"));
        assert!(!is_safe_relative_path("a\\b.txt"));
        assert!(!is_safe_relative_path("https://evil.example.com/x"));
        assert!(!is_safe_relative_path("a/b?query=1"));
        assert!(!is_safe_relative_path("a/b#frag"));
        assert!(!is_safe_relative_path(""));
        assert!(!is_safe_relative_path("./x.txt"));
    }

    #[test]
    fn safe_segment_rules() {
        assert!(is_safe_segment("demo"));
        assert!(is_safe_segment("Demo-1.0_x"));
        assert!(!is_safe_segment(""));
        assert!(!is_safe_segment("."));
        assert!(!is_safe_segment(".."));
        assert!(!is_safe_segment("a/b"));
        assert!(!is_safe_segment("a\\b"));
    }

    #[derive(Clone)]
    pub(super) struct Catalog {
        pub(super) index: String,
        pub(super) files: HashMap<String, String>,
        #[allow(dead_code)]
        pub(super) hits: HashMap<String, usize>,
    }

    pub(super) fn catalog_server() -> (String, std::sync::Arc<Mutex<Catalog>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let state = std::sync::Arc::new(Mutex::new(Catalog {
            index: String::new(),
            files: HashMap::new(),
            hits: HashMap::new(),
        }));
        let shared = state.clone();
        std::thread::spawn(move || {
            let server = tiny_http::Server::from_listener(listener, None).unwrap();
            for request in server.incoming_requests() {
                let path = request.url().to_string();
                let catalog = shared.lock().unwrap().clone();
                let reply = if path == "/index.json" {
                    Some(catalog.index.clone())
                } else {
                    catalog.files.get(path.trim_start_matches('/')).cloned()
                };
                match reply {
                    Some(content) => {
                        if let Ok(mut guard) = shared.lock() {
                            *guard.hits.entry(request.url().to_string()).or_default() += 1;
                        }
                        let mut response = tiny_http::Response::from_string(content);
                        response.add_header(
                            tiny_http::Header::from_bytes(&b"content-type"[..], &b"application/json"[..]).unwrap(),
                        );
                        let _ = request.respond(response);
                    }
                    None => {
                        let _ = request.respond(tiny_http::Response::from_string("not found").with_status_code(404));
                    }
                }
            }
        });
        std::thread::sleep(Duration::from_millis(50));
        (url, state)
    }

    pub(super) fn demo_catalog() -> Catalog {
        let mut files = HashMap::new();
        files.insert("demo/SKILL.md".to_string(), "---\nname: Demo\ndescription: demo\n---\nBody A\n".to_string());
        files.insert("demo/extra.txt".to_string(), "extra-a".to_string());
        Catalog {
            index: r#"{"skills":[{"name":"demo","version":"1","files":["SKILL.md","extra.txt"]}]}"#.to_string(),
            files,
            hits: HashMap::new(),
        }
    }

    #[test]
    fn pull_downloads_catalog_into_cache() {
        let dir = cache_dir("happy");
        let (url, state) = catalog_server();
        *state.lock().unwrap() = demo_catalog();
        let roots = pull(&dir, &url).expect("pull");
        assert_eq!(roots.len(), 1);
        assert!(roots[0].ends_with("demo"));
        let content = std::fs::read_to_string(roots[0].join("SKILL.md")).unwrap();
        assert!(content.contains("Body A"));
        assert_eq!(std::fs::read_to_string(roots[0].join("extra.txt")).unwrap(), "extra-a");
        assert_eq!(std::fs::read_to_string(roots[0].join(".devcode-version")).unwrap(), "1");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pull_same_version_does_not_refetch_files() {
        let dir = cache_dir("cached");
        let (url, state) = catalog_server();
        *state.lock().unwrap() = demo_catalog();
        pull(&dir, &url).expect("first pull");
        // Change what the server would serve; same version means no re-download.
        {
            let mut guard = state.lock().unwrap();
            guard.files.insert("demo/SKILL.md".to_string(), "CHANGED".to_string());
        }
        let roots = pull(&dir, &url).expect("second pull");
        assert_eq!(roots.len(), 1);
        let content = std::fs::read_to_string(roots[0].join("SKILL.md")).unwrap();
        assert!(content.contains("Body A"), "same-version pull must reuse the cache: {content}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pull_refreshes_on_version_bump() {
        let dir = cache_dir("refresh");
        let (url, state) = catalog_server();
        *state.lock().unwrap() = demo_catalog();
        pull(&dir, &url).expect("first pull");
        {
            let mut guard = state.lock().unwrap();
            guard.index = r#"{"skills":[{"name":"demo","version":"2","files":["SKILL.md","extra.txt"]}]}"#.to_string();
            guard.files.insert("demo/SKILL.md".to_string(), "---\nname: Demo\n---\nBody B\n".to_string());
            guard.files.insert("demo/extra.txt".to_string(), "extra-b".to_string());
        }
        let roots = pull(&dir, &url).expect("second pull");
        assert_eq!(roots.len(), 1);
        assert!(std::fs::read_to_string(roots[0].join("SKILL.md")).unwrap().contains("Body B"));
        assert_eq!(std::fs::read_to_string(roots[0].join("extra.txt")).unwrap(), "extra-b");
        assert_eq!(std::fs::read_to_string(roots[0].join(".devcode-version")).unwrap(), "2");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pull_rejects_unsafe_entries() {
        let dir = cache_dir("unsafe");
        let (url, state) = catalog_server();
        let mut files = HashMap::new();
        files.insert("ok/SKILL.md".to_string(), "---\nname: Ok\n---\nok\n".to_string());
        *state.lock().unwrap() = Catalog {
            index: r#"{"skills":[
                {"name":"traversal","files":["../escape.txt","SKILL.md"]},
                {"name":"absolute","files":["/etc/passwd","SKILL.md"]},
                {"name":"crossed","files":["SKILL.md","http://127.0.0.1/other.txt"]},
                {"name":"nope/slash","files":["SKILL.md"]},
                {"name":"no-skill-md","files":["other.txt"]},
                {"name":"ok","files":["SKILL.md"]}
            ]}"#
            .to_string(),
            files,
            hits: HashMap::new(),
        };
        let roots = pull(&dir, &url).expect("pull");
        let names: Vec<String> = roots
            .iter()
            .filter_map(|root| root.file_name().map(|name| name.to_string_lossy().into_owned()))
            .collect();
        assert_eq!(names, vec!["ok"]);
        assert!(!dir.join("escape.txt").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pull_unreachable_index_is_err() {
        // Bind then drop the listener so the port refuses connections.
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            drop(listener);
            port
        };
        let dir = cache_dir("unreachable");
        let result = pull(&dir, &format!("http://127.0.0.1:{port}/"));
        assert!(result.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod load_tests {
    use super::tests::*;
    use super::*;
    use crate::{load, Skill};

    fn write_file(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn ids(skills: &[Skill]) -> Vec<&str> {
        skills.iter().map(|skill| skill.id.as_str()).collect()
    }

    fn configured(dir: &Path, jsonc: &str) {
        let config = dir.join("config");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::write(config.join("devcode.jsonc"), jsonc).unwrap();
    }

    fn run_paths(dir: &Path) -> devcode_util::Paths {
        devcode_util::Paths {
            home: dir.join("home"),
            data: dir.join("data"),
            cache: dir.join("cache"),
            config: dir.join("config"),
            state: dir.join("state"),
            tmp: dir.join("tmp"),
        }
    }

    #[test]
    fn load_merges_absolute_relative_and_remote_sources() {
        let dir = std::env::temp_dir().join(format!("devcode-skills-sources-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let run = dir.join("run");
        // Absolute-path source.
        write_file(
            &dir.join("abs-src").join("absskill").join("SKILL.md"),
            "---\nname: Abs\ndescription: abs\n---\nabs\n",
        );
        // Relative-to-run-directory source.
        write_file(
            &run.join("rel-src").join("relskill").join("SKILL.md"),
            "---\nname: Rel\ndescription: rel\n---\nrel\n",
        );
        // Remote source served over the loopback.
        let (url, state) = catalog_server();
        *state.lock().unwrap() = demo_catalog();
        configured(
            &dir,
            &format!(
                r#"{{ "skills": ["{}", "./rel-src", "{url}"] }}"#,
                dir.join("abs-src").display()
            ),
        );
        let paths = run_paths(&dir);
        let skills = load(&paths, &run, "0.1.0-rust");
        let names = ids(&skills);
        assert!(names.contains(&"absskill"), "missing absolute source: {names:?}");
        assert!(names.contains(&"relskill"), "missing relative source: {names:?}");
        assert!(names.contains(&"demo"), "missing remote source: {names:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn legacy_object_form_loads_paths_then_urls() {
        let dir = std::env::temp_dir().join(format!("devcode-skills-legacy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        write_file(
            &dir.join("legacy-src").join("legacyskill").join("SKILL.md"),
            "---\nname: Legacy\ndescription: legacy\n---\nlegacy\n",
        );
        let (url, state) = catalog_server();
        *state.lock().unwrap() = demo_catalog();
        configured(
            &dir,
            &format!(
                r#"{{ "skills": {{ "paths": ["{}"], "urls": ["{url}"] }} }}"#,
                dir.join("legacy-src").display()
            ),
        );
        let paths = run_paths(&dir);
        let skills = load(&paths, &dir, "0.1.0-rust");
        let names = ids(&skills);
        assert!(names.contains(&"legacyskill"), "missing legacy path: {names:?}");
        assert!(names.contains(&"demo"), "missing legacy url: {names:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
