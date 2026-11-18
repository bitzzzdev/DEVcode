//! Project identity and persistence, mirroring the common path of
//! packages/core/src/project.ts and packages/schema/src/project*.ts.
//! Git: sha1 of the normalized origin remote, falling back to the root commit.
//! Non-git directory: sha1 of "directory:<path>".

use std::path::{Component, Path, PathBuf};
use std::process::Command;

use rusqlite::{params, Connection};
use sha1::{Digest, Sha1};

#[derive(Debug, Clone, PartialEq)]
pub struct Resolved {
    pub id: String,
    pub directory: String,
    pub canonical: String,
    pub vcs: Option<String>,
}

pub fn resolve(directory: &Path) -> Resolved {
    let directory = absolute(directory).to_string_lossy().into_owned();
    if let Some(native) = find_up(Path::new(&directory), &[".git", ".hg"]) {
        let name = native.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name == ".git" {
            if let Some(resolved) = git_resolve(&native, &directory) {
                return resolved;
            }
        }
        if name == ".hg" {
            if let Some(node) = command_stdout(
                dot_parent(&native),
                &["hg", "log", "-r", "roots(all())", "-T", "{node}\n"],
                &[("HGPLAIN", "1")],
            )
            .and_then(|stdout| {
                stdout
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .min()
                    .map(str::to_owned)
            }) {
                return Resolved {
                    id: node,
                    directory: directory.clone(),
                    canonical: directory.clone(),
                    vcs: Some("hg".into()),
                };
            }
        }
    }
    Resolved {
        id: sha1(&format!("directory:{directory}")),
        canonical: directory.clone(),
        directory,
        vcs: None,
    }
}

/// Upsert the project row. First write wins on identity fields; canonical and
/// vcs are refreshed on every resolve, like the bun build's persist.
pub fn persist(conn: &Connection, resolved: &Resolved, now: i64) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO project (id, worktree, vcs, name, time_created, time_updated, time_active, sandboxes) \
         VALUES (?1, ?2, ?3, NULL, ?4, ?4, ?4, '[]') \
         ON CONFLICT(id) DO UPDATE SET worktree = excluded.worktree, vcs = excluded.vcs, time_updated = excluded.time_updated, time_active = excluded.time_active",
        params![resolved.id, resolved.canonical, resolved.vcs, now],
    )?;
    Ok(())
}

fn git_resolve(dot_git: &Path, directory: &str) -> Option<Resolved> {
    let repo_dir = dot_parent(dot_git);
    let toplevel = command_stdout(repo_dir, &["git", "rev-parse", "--show-toplevel"], &[])?
        .trim()
        .to_owned();
    if toplevel.is_empty() {
        return None;
    }
    let common_dir = command_stdout(repo_dir, &["git", "rev-parse", "--git-common-dir"], &[])
        .unwrap_or_default()
        .trim()
        .to_owned();
    let is_main = common_dir.is_empty()
        || Path::new(&common_dir) == Path::new(".git")
        || Path::new(&common_dir).canonicalize().ok() == dot_git.canonicalize().ok();
    let canonical = if is_main {
        toplevel.clone()
    } else {
        // Linked worktree: canonical is the main worktree's root.
        command_stdout(repo_dir, &["git", "worktree", "list", "--porcelain"], &[])
            .and_then(|porcelain| {
                porcelain
                    .lines()
                    .find_map(|line| line.strip_prefix("worktree ").map(str::to_owned))
            })
            .unwrap_or_else(|| toplevel.clone())
    };
    let id = command_stdout(repo_dir, &["git", "remote", "get-url", "origin"], &[])
        .and_then(|url| normalize_remote(&url))
        .map(|normalized| sha1(&format!("git-remote:{normalized}")))
        .or_else(|| {
            command_stdout(
                repo_dir,
                &["git", "rev-list", "--max-parents=0", "HEAD"],
                &[],
            )
            .and_then(|stdout| {
                stdout
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .min()
                    .map(str::to_owned)
            })
        })
        .unwrap_or_else(|| "global".to_owned());
    Some(Resolved {
        id,
        directory: directory.to_owned(),
        canonical,
        vcs: Some("git".into()),
    })
}

/// Normalize a git remote URL to `host/path` (no scheme, credentials, or `.git` suffix),
/// matching packages/core/src/project.ts `url`/`parts`.
fn normalize_remote(raw: &str) -> Option<String> {
    let value = raw.trim();
    if value.is_empty() {
        return None;
    }
    let rest = value.split_once("://").map(|(_, rest)| rest);
    let (host, path) = match rest {
        Some(rest) => {
            if value.starts_with("file:") {
                return None;
            }
            let without_auth = rest.rsplit_once('@').map(|(_, host)| host).unwrap_or(rest);
            without_auth
                .split_once('/')
                .map(|(host, path)| (host.to_owned(), path.to_owned()))?
        }
        None => {
            // SCP-like SSH: [user@]host:path
            let (host_part, path) = value.split_once(':')?;
            let host = host_part
                .rsplit_once('@')
                .map(|(_, host)| host)
                .unwrap_or(host_part);
            if host.is_empty() || host.contains('/') {
                return None;
            }
            (host.to_owned(), path.to_owned())
        }
    };
    let path = path
        .trim_start_matches('/')
        .trim_end_matches('/')
        .trim_end_matches(".git");
    if host.is_empty() || path.is_empty() {
        return None;
    }
    Some(format!("{}/{}", host.to_ascii_lowercase(), path))
}

fn absolute(directory: &Path) -> PathBuf {
    let absolute = if directory.is_absolute() {
        directory.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(directory)
    };
    absolute
        .components()
        .filter(|component| *component != Component::CurDir)
        .collect()
}

fn find_up(start: &Path, targets: &[&str]) -> Option<PathBuf> {
    let mut current = Some(start);
    while let Some(dir) = current {
        for target in targets {
            let candidate = dir.join(target);
            if candidate.exists() {
                return Some(candidate);
            }
        }
        current = dir.parent();
    }
    None
}

fn dot_parent(dot: &Path) -> &Path {
    dot.parent().unwrap_or(dot)
}

fn command_stdout(cwd: &Path, argv: &[&str], env: &[(&str, &str)]) -> Option<String> {
    let mut command = Command::new(argv[0]);
    command.args(&argv[1..]).current_dir(cwd);
    for (key, value) in env {
        command.env(key, value);
    }
    let output = command.output().ok()?;
    if output.status.success() {
        String::from_utf8(output.stdout).ok()
    } else {
        None
    }
}

fn sha1(input: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(input.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_https_remote_urls() {
        assert_eq!(
            normalize_remote("https://github.com/Bitzzdev/DEVcode.git"),
            Some("github.com/Bitzzdev/DEVcode".into())
        );
        assert_eq!(
            normalize_remote("https://user:pass@github.com/Bitzzdev/DEVcode/"),
            Some("github.com/Bitzzdev/DEVcode".into())
        );
        assert_eq!(
            normalize_remote("git@github.com:Bitzzdev/DEVcode.git"),
            Some("github.com/Bitzzdev/DEVcode".into())
        );
        assert_eq!(
            normalize_remote("git://github.com/Bitzzdev/DEVcode.git"),
            Some("github.com/Bitzzdev/DEVcode".into())
        );
        assert_eq!(normalize_remote("file:///tmp/repo"), None);
        assert_eq!(normalize_remote(""), None);
    }

    #[test]
    fn non_repo_directory_resolves_to_stable_hash() {
        let dir =
            std::env::temp_dir().join(format!("devcode-nogit-{}", crate::identifier::ascending()));
        std::fs::create_dir_all(&dir).unwrap();
        let resolved = resolve(&dir);
        assert_eq!(resolved.vcs, None);
        assert_eq!(
            resolved.id,
            sha1(&format!("directory:{}", resolved.directory))
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
