//! Core model-facing tools for the Rust build. Ports the `read`, `write`, `edit`,
//! `glob`, `grep` and `shell` leaves from packages/core/src/tool/plugin/.
//!
//! Deliberate deviations from the Bun build:
//! - `edit` implements exact-match replacement (unique or replaceAll) only. The
//!   Bun build additionally retries with trailing-whitespace tiers and
//!   normalized quotes/dashes/spaces, and normalizes line endings per region;
//!   those tiers are not ported yet.
//! - `read` does not attach images/PDFs and does not honor `devcode://skill` URIs.
//! - `shell` runs `sh -c` with a wall-clock timeout that kills the process;
//!   unlike the Bun build there is no move-to-background handoff on timeout
//!   (background=true is supported explicitly instead).
//! - No permission prompts here yet; callers gate access. Output is capped at
//!   50 KiB / 2000 lines (packages/core/src/tool-output.ts); overflow text is
//!   written to the tool-output directory and its path is returned.

use std::fmt::Write as _;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const MAX_BYTES: usize = 50 * 1024;
pub const MAX_LINES: usize = 2_000;
pub const SHELL_DEFAULT_TIMEOUT_MS: u64 = 2 * 60 * 1_000;
pub const SEARCH_DEFAULT_LIMIT: usize = 100;

#[derive(Debug)]
pub enum ToolError {
    Io(std::io::Error),
    NotFound(String),
    /// Repeated occurrences without an explicit replace_all.
    Ambiguous(usize),
    /// old_string did not match anywhere.
    NoMatch,
    Message(String),
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ToolError::Io(error) => write!(f, "{error}"),
            ToolError::NotFound(message) => write!(f, "{message}"),
            ToolError::Ambiguous(count) => write!(
                f,
                "Found {count} occurrences of oldString; expected exactly one. Use replaceAll to replace every occurrence."
            ),
            ToolError::NoMatch => write!(f, "No occurrences of oldString found; each character including whitespace and line endings must match exactly."),
            ToolError::Message(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for ToolError {}

impl From<std::io::Error> for ToolError {
    fn from(error: std::io::Error) -> Self {
        ToolError::Io(error)
    }
}

/// Truncate `text` to at most `max_bytes` bytes at a char boundary, keeping the
/// tail (errors and recent output matter most). Returns the possibly-shortened
/// text and whether truncation happened.
pub fn cap_tail(text: &str, max_bytes: usize) -> (String, bool) {
    if text.len() <= max_bytes {
        return (text.to_owned(), false);
    }
    let keep = text.len() - max_bytes;
    let boundary = text
        .char_indices()
        .find(|(index, _)| *index >= keep)
        .map(|(index, _)| index)
        .unwrap_or(text.len());
    (
        format!(
            "... (truncated, showing last {max_bytes} bytes)\n{}",
            &text[boundary..]
        ),
        true,
    )
}

pub struct Cap {
    pub text: String,
    pub truncated: bool,
    pub overflow_path: Option<PathBuf>,
}

/// Cap output to MAX_BYTES/MAX_LINES; overflow is retained on disk and its path returned.
pub fn cap_output(paths: &devcode_util::Paths, full: &str) -> Cap {
    let mut lines: Vec<&str> = full.lines().collect();
    let mut truncated = false;
    if lines.len() > MAX_LINES {
        lines = lines[lines.len() - MAX_LINES..].to_vec();
        truncated = true;
    }
    let joined = lines.join("\n");
    let (text, bytes_truncated) = cap_tail(&joined, MAX_BYTES);
    if bytes_truncated {
        truncated = true;
    }
    if !truncated {
        return Cap {
            text,
            truncated: false,
            overflow_path: None,
        };
    }
    let directory = paths.data.join("tool-output");
    let overflow_path = fs::create_dir_all(&directory).ok().and_then(|_| {
        let path = directory.join(format!(
            "tool_{}.log",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()?
                .as_millis()
        ));
        fs::write(&path, full).ok()?;
        Some(path)
    });
    Cap {
        text,
        truncated: true,
        overflow_path,
    }
}

pub struct ReadOutput {
    pub text: String,
    /// Number of lines/entries shown.
    pub count: usize,
    pub total_lines: Option<usize>,
}

pub fn read(
    path: &Path,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<ReadOutput, ToolError> {
    let limit = limit.unwrap_or(MAX_LINES).min(MAX_LINES);
    if limit == 0 {
        return Err(ToolError::Message(
            "Limit must be a positive integer".into(),
        ));
    }
    if !path.exists() {
        return Err(ToolError::NotFound(format!(
            "File not found: {}",
            path.display()
        )));
    }
    if path.is_dir() {
        let mut entries: Vec<String> = fs::read_dir(path)?
            .filter_map(|entry| entry.ok())
            .map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    format!("{name}/")
                } else {
                    name
                }
            })
            .collect();
        entries.sort();
        let total = entries.len();
        let start = offset.unwrap_or(1).saturating_sub(1);
        let shown: Vec<String> = entries.into_iter().skip(start).take(limit).collect();
        let count = shown.len();
        return Ok(ReadOutput {
            text: shown.join("\n"),
            count,
            total_lines: Some(total),
        });
    }
    let content = fs::read_to_string(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::InvalidData {
            ToolError::Message(format!("Not a text file: {}", path.display()))
        } else {
            ToolError::Io(error)
        }
    })?;
    let lines: Vec<&str> = content.lines().collect();
    let total = lines.len();
    let start = offset.unwrap_or(1).saturating_sub(1);
    let mut out = String::new();
    for (index, line) in lines.iter().enumerate().skip(start).take(limit) {
        writeln!(out, "{}: {}", index + 1, line).expect("string write");
    }
    let count = lines.iter().enumerate().skip(start).take(limit).count();
    Ok(ReadOutput {
        text: out.trim_end().to_owned(),
        count,
        total_lines: Some(total),
    })
}

pub fn write(path: &Path, content: &str) -> Result<String, ToolError> {
    let existed = path.exists();
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    fs::write(path, content)?;
    Ok(if existed {
        format!("Wrote file successfully: {}", path.display())
    } else {
        format!("Created file successfully: {}", path.display())
    })
}

pub fn edit(
    path: &Path,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
) -> Result<String, ToolError> {
    if old_string == new_string {
        return Err(ToolError::Message(
            "newString must differ from oldString".into(),
        ));
    }
    let content = fs::read_to_string(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            ToolError::NotFound(format!("File not found: {}", path.display()))
        } else {
            ToolError::Io(error)
        }
    })?;
    if old_string.is_empty() {
        return Err(ToolError::Message("oldString must not be empty".into()));
    }
    let occurrences = content.matches(old_string).count();
    if occurrences == 0 {
        return Err(ToolError::NoMatch);
    }
    if occurrences > 1 && !replace_all {
        return Err(ToolError::Ambiguous(occurrences));
    }
    let updated = if replace_all {
        content.replace(old_string, new_string)
    } else {
        content.replacen(old_string, new_string, 1)
    };
    fs::write(path, updated)?;
    let applied = if replace_all { occurrences } else { 1 };
    Ok(format!(
        "Applied {applied} replacement(s) to {}",
        path.display()
    ))
}

pub fn glob(
    pattern: &str,
    root: &Path,
    hidden: bool,
    limit: Option<usize>,
) -> Result<Vec<String>, ToolError> {
    let limit = limit.unwrap_or(SEARCH_DEFAULT_LIMIT);
    let matcher = globset::GlobBuilder::new(pattern)
        .literal_separator(false)
        .build()
        .map_err(|error| ToolError::Message(format!("Invalid glob pattern: {error}")))?
        .compile_matcher();
    let mut results = Vec::new();
    let mut walker = ignore::WalkBuilder::new(root);
    walker
        .hidden(!hidden)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true);
    for entry in walker.build().flatten() {
        let relative = entry.path().strip_prefix(root).unwrap_or(entry.path());
        let candidate = relative.to_string_lossy().replace('\\', "/");
        if matcher.is_match(&candidate) {
            results.push(entry.path().to_string_lossy().into_owned());
            if results.len() >= limit {
                break;
            }
        }
    }
    results.sort();
    Ok(results)
}

pub struct GrepMatch {
    pub path: String,
    pub line: usize,
    pub text: String,
}

pub fn grep(
    pattern: &str,
    path: &Path,
    include: Option<&str>,
    literal: bool,
    case_sensitive: bool,
    limit: Option<usize>,
) -> Result<Vec<GrepMatch>, ToolError> {
    let limit = limit.unwrap_or(SEARCH_DEFAULT_LIMIT);
    if pattern.is_empty() {
        return Err(ToolError::Message("Pattern must not be empty".into()));
    }
    let escaped;
    let source = if literal {
        escaped = regex::escape(pattern);
        escaped.as_str()
    } else {
        pattern
    };
    let regex = regex::RegexBuilder::new(source)
        .case_insensitive(!case_sensitive)
        .build()
        .map_err(|error| ToolError::Message(format!("Invalid pattern: {error}")))?;
    let include_matcher = match include {
        Some(glob) => Some(
            globset::GlobBuilder::new(glob)
                .literal_separator(false)
                .build()
                .map_err(|error| ToolError::Message(format!("Invalid include glob: {error}")))?
                .compile_matcher(),
        ),
        None => None,
    };
    let mut matches = Vec::new();
    let mut files = Vec::new();
    if path.is_file() {
        files.push(path.to_path_buf());
    } else {
        let mut walker = ignore::WalkBuilder::new(path);
        walker
            .hidden(true)
            .git_ignore(true)
            .git_global(true)
            .git_exclude(true);
        files.extend(
            walker
                .build()
                .flatten()
                .map(|entry| entry.path().to_path_buf()),
        );
    }
    for file in files {
        if !file.is_file() {
            continue;
        }
        if let Some(matcher) = &include_matcher {
            let name = file
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            if !matcher.is_match(&name) {
                continue;
            }
        }
        let Ok(bytes) = fs::read(&file) else { continue };
        if bytes.iter().take(8192).any(|byte| *byte == 0) {
            continue; // binary
        }
        let Ok(content) = String::from_utf8(bytes) else {
            continue;
        };
        for (index, line) in content.lines().enumerate() {
            if regex.is_match(line) {
                matches.push(GrepMatch {
                    path: file.to_string_lossy().into_owned(),
                    line: index + 1,
                    text: line.to_owned(),
                });
                if matches.len() >= limit {
                    return Ok(matches);
                }
            }
        }
    }
    Ok(matches)
}

/// TS-style model content for grep results.
pub fn grep_to_model_content(matches: &[GrepMatch], truncated: bool, limit: usize) -> String {
    let mut lines = if matches.is_empty() {
        vec!["No matches found".to_owned()]
    } else {
        vec![format!("Found {} matches", matches.len())]
    };
    let mut current = String::new();
    for match_ in matches {
        if current != match_.path {
            if !current.is_empty() {
                lines.push(String::new());
            }
            current = match_.path.clone();
            lines.push(format!("{}:", match_.path));
        }
        lines.push(format!("  Line {}: {}", match_.line, match_.text));
    }
    if truncated {
        lines.push(String::new());
        lines.push(format!("(Results are truncated: showing first {limit} results. Consider using a more specific path or pattern.)"));
    }
    lines.join("\n")
}

pub struct ShellOutput {
    pub output: String,
    pub exit_code: Option<i32>,
    pub wall_time_seconds: f64,
    pub timed_out: bool,
    pub overflow_path: Option<PathBuf>,
    pub pid: Option<u32>,
}

#[cfg(unix)]
pub fn shell(
    paths: &devcode_util::Paths,
    command: &str,
    workdir: Option<&Path>,
    timeout_ms: Option<u64>,
    session_id: Option<&str>,
) -> Result<ShellOutput, ToolError> {
    let started = Instant::now();
    let timeout = Duration::from_millis(timeout_ms.unwrap_or(SHELL_DEFAULT_TIMEOUT_MS));
    let spawn = |grouped: bool| {
        let mut child = Command::new("sh");
        child
            .arg("-c")
            .arg(command)
            .current_dir(workdir.unwrap_or_else(|| Path::new(".")))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("DEVCODE_SESSION_ID", session_id.unwrap_or(""));
        if grouped {
            use std::os::unix::process::CommandExt;
            child.process_group(0); // so the timeout can kill the whole command tree
        }
        child.spawn()
    };
    // Sandboxed environments may deny the process-group setup; fall back to a plain spawn.
    let (mut child, grouped) = match spawn(true) {
        Ok(child) => (child, true),
        Err(_) => (spawn(false)?, false),
    };

    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let reader = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = stdout.read_to_end(&mut buffer);
        let mut err = Vec::new();
        let _ = stderr.read_to_end(&mut err);
        (buffer, err)
    });

    let pid = Some(child.id());
    let (exit_code, timed_out) = loop {
        match child.try_wait()? {
            Some(status) => break (status.code(), false),
            None => {
                if started.elapsed() > timeout {
                    if grouped {
                        // Kill the whole process group; negative pid signals the group.
                        let pid_raw = child.id() as i32;
                        unsafe { libc::kill(-pid_raw, libc::SIGKILL) };
                    } else {
                        let _ = child.kill();
                    }
                    let _ = child.wait();
                    break (None, true);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    };
    let (out, err) = reader.join().unwrap_or_default();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out),
        String::from_utf8_lossy(&err)
    );
    let capped = cap_output(paths, &combined);
    Ok(ShellOutput {
        output: capped.text,
        exit_code,
        wall_time_seconds: started.elapsed().as_secs_f64(),
        timed_out,
        overflow_path: capped.overflow_path,
        pid,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "devcode-tools-{tag}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn paths(tag: &str) -> (PathBuf, devcode_util::Paths) {
        let root = temp_dir(tag);
        let paths = devcode_util::Paths {
            home: root.clone(),
            config: root.clone(),
            state: root.clone(),
            data: root.join("data"),
            cache: root.clone(),
            tmp: root.clone(),
        };
        (root, paths)
    }

    #[test]
    fn read_numbers_lines_and_honors_offset_limit() {
        let dir = temp_dir("read");
        let file = dir.join("sample.txt");
        fs::write(&file, "one\ntwo\nthree\nfour\n").unwrap();
        let out = read(&file, Some(2), Some(2)).unwrap();
        assert_eq!(out.text, "2: two\n3: three");
        assert_eq!(out.count, 2);
        assert_eq!(out.total_lines, Some(4));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn read_directory_lists_entries_with_trailing_slash() {
        let dir = temp_dir("readdir");
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join("file.txt"), "x").unwrap();
        let out = read(&dir, None, None).unwrap();
        assert!(out.text.contains("sub/"));
        assert!(out.text.contains("file.txt"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn read_missing_and_zero_limit_error() {
        let dir = temp_dir("readerr");
        assert!(matches!(
            read(&dir.join("nope"), None, None),
            Err(ToolError::NotFound(_))
        ));
        assert!(matches!(
            read(&dir, None, Some(0)),
            Err(ToolError::Message(_))
        ));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn write_creates_parents_and_reports_created_vs_wrote() {
        let dir = temp_dir("write");
        let file = dir.join("nested/deeper/file.txt");
        assert!(write(&file, "hello")
            .unwrap()
            .starts_with("Created file successfully"));
        assert_eq!(fs::read_to_string(&file).unwrap(), "hello");
        assert!(write(&file, "hello2")
            .unwrap()
            .starts_with("Wrote file successfully"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn edit_requires_unique_match_without_replace_all() {
        let dir = temp_dir("edit");
        let file = dir.join("edit.txt");
        fs::write(&file, "aaa bbb aaa").unwrap();
        assert!(matches!(
            edit(&file, "aaa", "ccc", false),
            Err(ToolError::Ambiguous(2))
        ));
        assert!(edit(&file, "aaa", "ccc", true).is_ok());
        assert_eq!(fs::read_to_string(&file).unwrap(), "ccc bbb ccc");
        assert!(matches!(
            edit(&file, "missing", "x", false),
            Err(ToolError::NoMatch)
        ));
        assert!(matches!(
            edit(&file, "ccc", "ccc", true),
            Err(ToolError::Message(_))
        ));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn glob_matches_and_limits() {
        let dir = temp_dir("glob");
        fs::write(dir.join("a.txt"), "x").unwrap();
        fs::write(dir.join("b.md"), "x").unwrap();
        fs::write(dir.join("c.txt"), "x").unwrap();
        let results = glob("*.txt", &dir, false, None).unwrap();
        assert_eq!(results.len(), 2);
        let limited = glob("*.txt", &dir, false, Some(1)).unwrap();
        assert_eq!(limited.len(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn grep_finds_lines_case_insensitively_and_skips_binary() {
        let dir = temp_dir("grep");
        fs::write(dir.join("hay.txt"), "Needle in a haystack\nneedle again\n").unwrap();
        fs::write(dir.join("bin.bin"), b"needle\0needle").unwrap();
        let matches = grep("needle", &dir, None, false, false, None).unwrap();
        assert_eq!(matches.len(), 2);
        assert!(matches.iter().all(|m| m.path.ends_with("hay.txt")));
        let sensitive = grep("Needle", &dir, None, false, true, None).unwrap();
        assert_eq!(sensitive.len(), 1);
        let literal = grep("needle again", &dir, None, true, true, None).unwrap();
        assert_eq!(literal.len(), 1);
        let limited = grep("needle", &dir, None, false, false, Some(1)).unwrap();
        assert_eq!(limited.len(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn grep_to_model_content_matches_ts_shape() {
        let matches = vec![GrepMatch {
            path: "a.txt".into(),
            line: 3,
            text: "hit".into(),
        }];
        let text = grep_to_model_content(&matches, false, 100);
        assert_eq!(text, "Found 1 matches\na.txt:\n  Line 3: hit");
        assert_eq!(grep_to_model_content(&[], false, 100), "No matches found");
    }

    #[test]
    fn cap_tail_keeps_last_bytes_at_char_boundary() {
        let text = "x".repeat(100) + " é" + &"y".repeat(100);
        let (capped, truncated) = cap_tail(&text, 50);
        assert!(truncated);
        assert!(capped.starts_with("... (truncated"));
        assert!(!capped.contains(" é"));
    }

    #[test]
    fn shell_captures_output_and_exit_code() {
        let (root, paths) = paths("shell");
        let out = shell(&paths, "echo hello && echo err >&2", None, None, None).unwrap();
        assert_eq!(out.exit_code, Some(0));
        assert!(out.output.contains("hello"));
        assert!(out.output.contains("err"));
        assert!(!out.timed_out);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn shell_timeout_kills_long_running_command() {
        let (root, paths) = paths("shelltimeout");
        let out = shell(&paths, "sleep 30", None, Some(100), None).unwrap();
        assert!(out.timed_out);
        assert_eq!(out.exit_code, None);
        assert!(out.wall_time_seconds < 10.0);
        std::fs::remove_dir_all(&root).unwrap();
    }
}

#[cfg(not(unix))]
pub fn shell(
    paths: &devcode_util::Paths,
    command: &str,
    workdir: Option<&Path>,
    timeout_ms: Option<u64>,
    session_id: Option<&str>,
) -> Result<ShellOutput, ToolError> {
    let started = Instant::now();
    let timeout = Duration::from_millis(timeout_ms.unwrap_or(SHELL_DEFAULT_TIMEOUT_MS));
    let output = Command::new("cmd")
        .arg("/C")
        .arg(command)
        .current_dir(workdir.unwrap_or_else(|| Path::new(".")))
        .env("DEVCODE_SESSION_ID", session_id.unwrap_or(""))
        .output()?;
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let capped = cap_output(paths, &combined);
    Ok(ShellOutput {
        output: capped.text,
        exit_code: output.status.code(),
        wall_time_seconds: started.elapsed().as_secs_f64(),
        timed_out: false,
        overflow_path: capped.overflow_path,
        pid: None,
    })
}
