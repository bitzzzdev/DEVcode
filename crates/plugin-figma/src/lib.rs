//! Figma plugin for the Rust build. Port of packages/plugins/figma (index.ts).
//!
//! Two contributions, both optional and additive:
//!   1. the `figma` remote MCP server (https://mcp.figma.com/mcp, OAuth via DCR —
//!      no shared client secret). An existing `mcp.servers.figma` entry in the
//!      user's config wins, including `disabled: true`.
//!   2. the official Figma skills, loaded from the vendored bundle. Nothing is
//!      re-authored; the bundle is Figma's material (see skills/NOTICE.md).
//!
//! Skill files are read from, in order:
//!   1. `$DEVCODE_FIGMA_SKILLS_DIR` (points at the `figma` directory containing
//!      `skills/` and `skills-source.json`)
//!   2. the `packages/plugins/figma` tree of this repository (compile-time path)
//! The skills list comes from `skills-source.json`, matching the Bun plugin.

use std::path::{Path, PathBuf};

use devcode_plugin::{Plugin, PluginInfo};
use devcode_skills::Skill;

pub struct Figma;

impl Plugin for Figma {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            id: "devcode.figma".into(),
            version: "2.3.0-rust".into(),
            description: "Figma remote MCP server and official Figma skills for DevCode".into(),
        }
    }

    fn mcp_servers(&self) -> Vec<(String, serde_json::Value)> {
        vec![(
            "figma".into(),
            serde_json::json!({
                "type": "remote",
                "url": "https://mcp.figma.com/mcp",
                "oauth": {},
            }),
        )]
    }

    fn skills(&self) -> Vec<Skill> {
        let Some(root) = figma_root() else {
            eprintln!("figma plugin: no skills bundle found (set DEVCODE_FIGMA_SKILLS_DIR)");
            return Vec::new();
        };
        let skills_dir = root.join("skills");
        let Some(ids) = skill_ids(&root) else {
            eprintln!("figma plugin: {} is unreadable or lists no skills", root.join("skills-source.json").display());
            return Vec::new();
        };
        ids.into_iter()
            .filter_map(|id| {
                let filepath = skills_dir.join(&id).join("SKILL.md");
                let content = std::fs::read_to_string(&filepath)
                    .map_err(|error| eprintln!("figma plugin: skipping {skill}: {error}", skill = filepath.display()))
                    .ok()?;
                let skill = devcode_skills::parse_skill_file(&skills_dir, &filepath, &content)?;
                Some(Skill {
                    id: id.clone(),
                    path: filepath.to_string_lossy().into_owned(),
                    ..skill
                })
            })
            .collect()
    }
}

fn figma_root() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("DEVCODE_FIGMA_SKILLS_DIR") {
        if !dir.is_empty() {
            return Some(PathBuf::from(dir));
        }
    }
    let in_tree = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../packages/plugins/figma")
        .canonicalize()
        .ok()?;
    in_tree.join("skills").is_dir().then_some(in_tree)
}

/// The pinned skill ids from skills-source.json. Falls back to scanning `skills/`
/// for SKILL.md directories when the manifest is missing or unreadable.
fn skill_ids(root: &Path) -> Option<Vec<String>> {
    let source = root.join("skills-source.json");
    let listed: Option<Vec<String>> = std::fs::read_to_string(&source)
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .and_then(|value| {
            value
                .get("skills")?
                .as_array()
                .map(|rows| rows.iter().filter_map(|row| row.as_str().map(str::to_owned)).collect())
        })
        .filter(|ids: &Vec<String>| !ids.is_empty());
    if let Some(ids) = listed {
        return Some(ids);
    }
    let scan = std::fs::read_dir(root.join("skills")).ok()?;
    let mut ids: Vec<String> = scan
        .flatten()
        .filter(|entry| entry.path().is_dir() && entry.path().join("SKILL.md").is_file())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    ids.sort();
    (!ids.is_empty()).then_some(ids)
}

// Same-toolchain Rust plugin trade-off: trait objects are not a stable C ABI.
#[allow(improper_ctypes_definitions)]
#[no_mangle]
pub extern "C" fn devcode_create_plugin() -> *mut dyn Plugin {
    Box::into_raw(Box::new(Figma))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registers_figma_remote_server() {
        let servers = Figma.mcp_servers();
        assert_eq!(servers.len(), 1);
        let (name, spec) = &servers[0];
        assert_eq!(name, "figma");
        assert_eq!(spec["type"], "remote");
        assert_eq!(spec["url"], "https://mcp.figma.com/mcp");
        // present and not false -> needs_oauth in load_config's parse_server
        assert!(spec.get("oauth").is_some());
    }

    #[test]
    fn loads_official_skills_from_the_bun_bundle() {
        let skills = Figma.skills();
        let ids: Vec<&str> = skills.iter().map(|skill| skill.id.as_str()).collect();
        assert!(
            ids.contains(&"figma-use") && ids.contains(&"figma-code-connect") && ids.contains(&"figma-design-to-code"),
            "expected the pinned official skills, got {ids:?}"
        );
        for skill in &skills {
            assert!(!skill.content.is_empty(), "{} has no body", skill.id);
            assert!(skill.path.ends_with("SKILL.md"), "{} path: {}", skill.id, skill.path);
            assert!(Path::new(&skill.path).is_file(), "{} path must exist", skill.id);
        }
    }
}
