//! Skills for the Rust build (port of packages/core/src/skill*.ts and
//! packages/core/src/config/plugin/skill-*.ts).
//!
//! Sources:
//!   1. Built-in skills embedded at compile time (devcode, report).
//!   2. `<config>/skills/**/SKILL.md` and `<config>/skill/**/SKILL.md` (BFS, max depth 6,
//!      dot directories skipped). A `.md` file directly inside those directories is
//!      also treated as a skill (Bun's depth-0 `.md` rule).
//!   3. Top-level `skills` in `<config>/devcode.jsonc`: paths (directories, `~/` expanded,
//!      relative resolved against the run directory, scanned like above) and http(s) URLs
//!      pulled into `<cache>/skills/<sha1(base)>/` (see remote.rs). The legacy object form
//!      `{ paths, urls }` is normalized to `[...paths, ...urls]`.
//!
//! Frontmatter is a deliberately small YAML subset: `---` fences, top-level
//! `key: value` pairs (optionally quoted) plus a single nested `metadata:` map
//! one level deep. Unknown keys and shapes are ignored, mirroring gray-matter's
//! tolerant parsing for the fields we consume. `metadata["devcode/autoinvoke"]`
//! overrides `disable-model-invocation`, same as the Bun build.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub mod remote;

const MAX_DEPTH: usize = 6;

#[derive(Debug, Clone, PartialEq)]
pub struct Skill {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub autoinvoke: Option<bool>,
    /// Absolute path of the SKILL.md file, or `skill://<id>/SKILL.md` for embedded skills.
    pub path: String,
    pub content: String,
}

/// Minimal frontmatter scan. Returns (frontmatter block, body).
fn split_frontmatter(content: &str) -> Option<(String, &str)> {
    let rest = content.strip_prefix("---")?;
    let rest = rest
        .strip_prefix('\n')
        .or_else(|| rest.strip_prefix("\r\n"))?;
    let end = rest.find("\n---")?;
    let front = rest[..end].to_string();
    let after = &rest[end + 4..];
    let body = after
        .strip_prefix('\n')
        .or_else(|| after.strip_prefix("\r\n"))
        .unwrap_or(after);
    Some((front, body))
}

fn parse_scalar(raw: &str) -> String {
    let value = raw.trim();
    if value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        return value[1..value.len() - 1].to_string();
    }
    // strip trailing comment only outside quotes
    match value.find(" #") {
        Some(index) => value[..index].trim_end().to_string(),
        None => value.to_string(),
    }
}

fn boolean_value(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Some(true),
        "false" | "no" | "off" | "0" => Some(false),
        _ => None,
    }
}

/// Parse the supported frontmatter keys. Top-level plus one nested `metadata:` map.
fn parse_frontmatter(block: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut nested_parent: Option<String> = None;
    for line in block.lines() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let indented = line.starts_with(' ') || line.starts_with('\t');
        let Some((key, raw_value)) = line.split_once(':') else {
            nested_parent = None;
            continue;
        };
        let key = key.trim().to_string();
        let raw_value = raw_value.trim();
        if !indented {
            nested_parent = None;
            if raw_value.is_empty() {
                nested_parent = Some(key.clone());
                continue;
            }
            out.insert(key, parse_scalar(raw_value));
            continue;
        }
        if let Some(parent) = &nested_parent {
            // one level only
            out.insert(
                format!("{parent}.{}", key_trim(&key)),
                parse_scalar(raw_value),
            );
        }
    }
    out
}

fn key_trim(key: &str) -> &str {
    key.trim()
}

pub fn parse_skill_file(directory: &Path, filepath: &Path, content: &str) -> Option<Skill> {
    let (frontmatter, body) = split_frontmatter(content)?;
    let map = parse_frontmatter(&frontmatter);
    let id = if filepath.parent() == Some(directory)
        && filepath.file_name() != Some(std::ffi::OsStr::new("SKILL.md"))
    {
        filepath.file_stem()?.to_string_lossy().into_owned()
    } else {
        filepath
            .parent()?
            .file_name()?
            .to_string_lossy()
            .into_owned()
    };
    let name = map.get("name").cloned().unwrap_or_else(|| id.clone());
    let description = map.get("description").cloned();
    let metadata_autoinvoke = map
        .get("metadata.devcode/autoinvoke")
        .and_then(|value| boolean_value(value));
    let disable = map
        .get("disable-model-invocation")
        .and_then(|value| boolean_value(value))
        .or_else(|| {
            map.get("disable-model-invocation")
                .and_then(|value| value.parse::<u64>().ok().map(|value| value == 1))
        });
    let autoinvoke = metadata_autoinvoke.or(disable.map(|disable| !disable));
    Some(Skill {
        id,
        name,
        description,
        autoinvoke,
        path: filepath.to_string_lossy().into_owned(),
        content: body.to_string(),
    })
}

/// Walk `root` for SKILL.md files (BFS). Depth 0 also yields top-level `*.md` files.
fn scan(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut queue = vec![(root.to_path_buf(), 0usize)];
    while let Some((directory, depth)) = queue.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        let mut children: Vec<(String, PathBuf, bool)> = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let path = entry.path();
            let is_dir = path.is_dir();
            children.push((name, path, is_dir));
        }
        children.sort_by(|a, b| a.0.cmp(&b.0));
        if let Some(skill) = children
            .iter()
            .find(|(name, _, is_dir)| !*is_dir && name == "SKILL.md")
        {
            files.push(skill.1.clone());
            continue;
        }
        if depth == 0 {
            files.extend(
                children
                    .iter()
                    .filter(|(name, _, is_dir)| !*is_dir && name.ends_with(".md"))
                    .map(|(_, path, _)| path.clone()),
            );
        }
        if depth < MAX_DEPTH {
            queue.extend(
                children
                    .into_iter()
                    .filter(|(_, _, is_dir)| *is_dir)
                    .map(|(_, path, _)| (path, depth + 1)),
            );
        }
    }
    files.sort();
    files
}

/// Embedded built-in skills. Content is `include_str!`'d from the Bun tree so there is
/// one source of truth; a missing file fails compilation loudly.
pub fn embedded_skills(app_version: &str) -> Vec<Skill> {
    let devcode_content = include_str!("../../../packages/core/src/plugin/skill/devcode.md");
    let report_body = include_str!("../../../packages/core/src/plugin/skill/report.md");
    let report_content = format!(
        "{report_body}\n## Runtime Diagnostics Snapshot\n\n\
         These values were captured when the built-in report skill was registered. Verify them before publishing.\n\n\
         - devcode version: {app_version}\n\
         - install/channel: rust\n\
         - OS: {} ({})\n\
         - Terminal: {}\n\
         - Shell: {}\n",
        std::env::consts::OS,
        std::env::consts::ARCH,
        [("TERM_PROGRAM", std::env::var("TERM_PROGRAM").ok()), ("TERM", std::env::var("TERM").ok()), ("COLORTERM", std::env::var("COLORTERM").ok())]
            .into_iter()
            .filter_map(|(key, value)| value.map(|value| format!("{key}={value}")))
            .collect::<Vec<_>>()
            .join(" "),
        std::env::var("SHELL").unwrap_or_else(|_| "Unavailable".to_string()),
    );
    vec![
        Skill {
            id: "devcode".into(),
            name: "DevCode".into(),
            description: Some(
                "Use this skill for any question about DevCode itself, including how DevCode works, using or configuring it, migrating from V1 to V2, troubleshooting it, developing plugins or integrations, using the DevCode SDK, clients, server, or API, and contributing to the DevCode codebase."
                    .into(),
            ),
            autoinvoke: None,
            path: "skill://devcode/SKILL.md".into(),
            content: devcode_content.to_string(),
        },
        Skill {
            id: "report".into(),
            name: "Report".into(),
            description: Some(
                "Use when the user wants to report a devcode issue or bug. Collect standard diagnostics, add user-specific reproduction context, and publish the issue with GitHub CLI."
                    .into(),
            ),
            autoinvoke: None,
            path: "skill://report/SKILL.md".into(),
            content: report_content,
        },
    ]
}

/// Load embedded skills plus everything under `<config>/skills/`.
fn scan_directory(dir: &Path, skills: &mut Vec<Skill>) {
    for filepath in scan(dir) {
        let Ok(content) = std::fs::read_to_string(&filepath) else {
            continue;
        };
        if let Some(skill) = parse_skill_file(dir, &filepath, &content) {
            // a skill loaded later overrides one with the same id
            skills.retain(|existing| existing.id != skill.id);
            skills.push(skill);
        }
    }
}

/// Configured skill sources from top-level `skills` in `<config>/devcode.jsonc`.
/// Array form: mixed paths and URLs in order. Legacy object form `{ paths, urls }`
/// is normalized to `[...paths, ...urls]` (Bun's `normalizeSkills`). URLs (http(s)
/// only) become remote sources; other entries expand `~/` against the home
/// directory, resolve relative to `directory`, and are scanned as directories.
fn configured_sources(config_file: &Path, paths: &devcode_util::Paths, directory: &Path) -> Vec<Source> {
    let Ok(raw) = std::fs::read_to_string(config_file) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&devcode_util::strip_jsonc(&raw)) else {
        eprintln!("failed to parse {}", config_file.display());
        return Vec::new();
    };
    let entries: Vec<String> = match value.get("skills") {
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .filter_map(serde_json::Value::as_str)
            .map(str::to_owned)
            .collect(),
        Some(serde_json::Value::Object(object)) => {
            let mut entries = Vec::new();
            for key in ["paths", "urls"] {
                if let Some(serde_json::Value::Array(items)) = object.get(key) {
                    entries.extend(items.iter().filter_map(serde_json::Value::as_str).map(str::to_owned));
                }
            }
            entries
        }
        _ => Vec::new(),
    };
    let mut seen = std::collections::HashSet::new();
    entries
        .into_iter()
        .filter_map(|entry| {
            if !seen.insert(entry.clone()) {
                return None;
            }
            if entry.to_ascii_lowercase().starts_with("http://") || entry.to_ascii_lowercase().starts_with("https://") {
                return Some(Source::Url(entry));
            }
            let expanded = if let Some(rest) = entry.strip_prefix("~/") {
                paths.home.join(rest)
            } else if Path::new(&entry).is_absolute() {
                PathBuf::from(&entry)
            } else {
                directory.join(&entry)
            };
            Some(Source::Directory(expanded))
        })
        .collect()
}

enum Source {
    Directory(PathBuf),
    Url(String),
}

pub fn load(paths: &devcode_util::Paths, directory: &Path, app_version: &str) -> Vec<Skill> {
    let mut skills = embedded_skills(app_version);
    // Bun loads `<dir>/skills` and `<dir>/skill` for every config directory; the Rust
    // port has a single config directory.
    for name in ["skills", "skill"] {
        scan_directory(&paths.config.join(name), &mut skills);
    }
    for source in configured_sources(&paths.config.join("devcode.jsonc"), paths, directory) {
        match source {
            Source::Directory(dir) => scan_directory(&dir, &mut skills),
            Source::Url(url) => {
                for root in remote::pull_cached(&paths.cache, &url) {
                    scan_directory(&root, &mut skills);
                }
            }
        }
    }
    skills
}

pub fn is_uri(path: &str) -> bool {
    path.starts_with("skill://")
}

fn directory_of(skill: &Skill) -> String {
    if is_uri(&skill.path) {
        format!("skill://{}", skill.id)
    } else {
        Path::new(&skill.path)
            .parent()
            .map(|parent| parent.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
}

/// Sampled file list for the `<skill_files>` block: up to 10 paths, sorted, SKILL.md excluded.
fn sample_files(skill: &Skill) -> Vec<String> {
    if is_uri(&skill.path) {
        return Vec::new();
    }
    let directory = Path::new(&skill.path)
        .parent()
        .unwrap_or(Path::new("/"))
        .to_path_buf();
    let mut files = Vec::new();
    let mut queue = vec![directory];
    while let Some(current) = queue.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || name == "SKILL.md" {
                continue;
            }
            let path = entry.path();
            if path.is_dir() {
                queue.push(path);
            } else {
                files.push(path.to_string_lossy().into_owned());
            }
        }
    }
    files.sort();
    files.truncate(10);
    files
}

pub fn to_model_output(skill: &Skill) -> String {
    let directory = directory_of(skill);
    let files = sample_files(skill);
    let mut lines = vec![
        format!("<skill_content name=\"{}\">", skill.name),
        format!("# Skill: {}", skill.name),
        String::new(),
        skill.content.trim().to_string(),
        String::new(),
        format!("Base directory for this skill: {directory}"),
        "Relative paths in this skill (e.g., scripts/, reference/) are relative to this base directory.".to_string(),
        "Note: file list is sampled.".to_string(),
        String::new(),
        "<skill_files>".to_string(),
    ];
    for file in files {
        lines.push(format!("<file>{file}</file>"));
    }
    lines.push("</skill_files>".to_string());
    lines.push("</skill_content>".to_string());
    lines.join("\n")
}

/// `<available_skills>` guidance block for the system prompt. Only skills that should
/// autoinvoke (autoinvoke != Some(false)) with a description are listed.
pub fn available_skills_section(skills: &[Skill]) -> String {
    let listed: Vec<&Skill> = skills
        .iter()
        .filter(|skill| skill.description.is_some() && skill.autoinvoke != Some(false))
        .collect();
    let mut lines = vec![
        "Skills provide specialized instructions and workflows for specific tasks.".to_string(),
        "Use the skill tool to load a skill when a task matches its description.".to_string(),
        "The user may also invoke a skill directly. When that happens, its instructions appear in the conversation as a <skill_content> block, the same shape the skill tool returns. A skill that is already present this way does not need to be invoked again.".to_string(),
    ];
    if listed.is_empty() {
        lines.push("No skills are currently available.".to_string());
        return lines.join("\n");
    }
    lines.push("<available_skills>".to_string());
    for skill in listed {
        lines.push("  <skill>".to_string());
        lines.push(format!("    <id>{}</id>", skill.id));
        lines.push(format!("    <name>{}</name>", skill.name));
        lines.push(format!(
            "    <description>{}</description>",
            skill.description.as_deref().unwrap_or("")
        ));
        lines.push("  </skill>".to_string());
    }
    lines.push("</available_skills>".to_string());
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_paths(dir: &Path) -> devcode_util::Paths {
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
    fn frontmatter_parsed_and_id_is_directory_name() {
        let dir = std::env::temp_dir().join(format!("devcode-skills-{}", std::process::id()));
        let paths = test_paths(&dir);
        let skill_dir = paths.config.join("skills").join("my-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: My Skill\ndescription: does things\ndisable-model-invocation: true\nmetadata:\n  devcode/autoinvoke: false\n---\n# Body\n\nDo the thing.\n",
        )
        .unwrap();
        let skills = load(&paths, &dir, "0.1.0-rust");
        let my = skills
            .iter()
            .find(|skill| skill.id == "my-skill")
            .expect("skill missing");
        assert_eq!(my.name, "My Skill");
        assert_eq!(my.description.as_deref(), Some("does things"));
        assert_eq!(my.autoinvoke, Some(false));
        assert!(my.content.contains("Do the thing."));
        // embedded skills always present
        assert!(skills.iter().any(|skill| skill.id == "devcode"));
        assert!(skills.iter().any(|skill| skill.id == "report"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn model_output_wraps_content_and_lists_files() {
        let dir = std::env::temp_dir().join(format!("devcode-skills-files-{}", std::process::id()));
        let paths = test_paths(&dir);
        let skill_dir = paths.config.join("skills").join("thing");
        std::fs::create_dir_all(skill_dir.join("scripts")).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: Thing\ndescription: x\n---\nBody text.\n",
        )
        .unwrap();
        std::fs::write(skill_dir.join("scripts").join("run.sh"), "echo hi\n").unwrap();
        let skills = load(&paths, &dir, "0.1.0-rust");
        let thing = skills.iter().find(|skill| skill.id == "thing").unwrap();
        let output = to_model_output(thing);
        assert!(output.starts_with("<skill_content name=\"Thing\">"));
        assert!(output.contains("Body text."));
        assert!(output.contains("run.sh"));
        assert!(output.ends_with("</skill_content>"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn available_skills_section_lists_autoinvoke_only() {
        let skills = vec![
            Skill {
                id: "yes".into(),
                name: "Yes".into(),
                description: Some("visible".into()),
                autoinvoke: None,
                path: "skill://yes/SKILL.md".into(),
                content: "body".into(),
            },
            Skill {
                id: "no".into(),
                name: "No".into(),
                description: Some("hidden".into()),
                autoinvoke: Some(false),
                path: "skill://no/SKILL.md".into(),
                content: "body".into(),
            },
        ];
        let section = available_skills_section(&skills);
        assert!(section.contains("<id>yes</id>"));
        assert!(!section.contains("<id>no</id>"));
    }
}
