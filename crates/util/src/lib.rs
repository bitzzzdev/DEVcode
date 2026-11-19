//! Shared paths and version for the Rust build. Mirrors packages/util/src/global-roots.ts
//! and the XDG layout the TypeScript build uses.

use std::path::{Path, PathBuf};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const CHANNEL: &str = "dev";

#[derive(Debug, Clone)]
pub struct Paths {
    pub home: PathBuf,
    pub data: PathBuf,
    pub cache: PathBuf,
    pub config: PathBuf,
    pub state: PathBuf,
    pub tmp: PathBuf,
}

fn xdg(var: &str, fallback: &Path) -> PathBuf {
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| fallback.to_path_buf())
}

/// Resolves the XDG-rooted paths for `app`, honoring DEVCODE_TEST_HOME (home override)
/// and DEVCODE_CONFIG_DIR (config override), matching the TypeScript build.
pub fn paths(app: &str) -> Paths {
    let home = std::env::var_os("DEVCODE_TEST_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."))
        });
    let tmp_root = std::env::temp_dir();
    let home = home.to_string_lossy().into_owned();
    let home_ref = Path::new(&home);
    let config = std::env::var_os("DEVCODE_CONFIG_DIR")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| xdg("XDG_CONFIG_HOME", &home_ref.join(".config")).join(app));
    Paths {
        home: PathBuf::from(&home),
        data: xdg("XDG_DATA_HOME", &home_ref.join(".local/share")).join(app),
        cache: xdg("XDG_CACHE_HOME", &home_ref.join(".cache")).join(app),
        config,
        state: xdg("XDG_STATE_HOME", &home_ref.join(".local/state")).join(app),
        tmp: tmp_root.join(app),
    }
}

/// Create the directories the Bun build's Global layer creates on startup
/// (data, cache, config, state, tmp). Best effort; called once by the binary.
pub fn ensure_dirs(paths: &Paths) {
    for directory in [&paths.data, &paths.cache, &paths.config, &paths.state, &paths.tmp] {
        let _ = std::fs::create_dir_all(directory);
    }
}

/// Strip `//` and `/* */` comments from JSONC, keeping string literals intact.
pub fn strip_jsonc(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            match c {
                '\\' => {
                    if let Some(next) = chars.next() {
                        out.push(next);
                    }
                }
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            '/' => match chars.peek() {
                Some('/') => {
                    for next in chars.by_ref() {
                        if next == '\n' {
                            out.push('\n');
                            break;
                        }
                    }
                }
                Some('*') => {
                    chars.next();
                    let mut prev = '\0';
                    for next in chars.by_ref() {
                        if prev == '*' && next == '/' {
                            break;
                        }
                        prev = next;
                    }
                }
                _ => out.push(c),
            },
            _ => out.push(c),
        }
    }
    out
}
