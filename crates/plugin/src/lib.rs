//! Native plugin host for the Rust build. Replaces the JS plugin runtime.
//!
//! A plugin is a `plugin.json` manifest next to a Rust dynamic library (cdylib).
//! The library exports `devcode_create_plugin() -> *mut dyn Plugin`. Loading and the
//! trait object layout rely on both sides being built by the same Rust toolchain;
//! this is the same trade spacy/cargo-expand style Rust plugin systems make and is
//! not a stable C ABI. The manifest, not the library, is the discovery contract.
//!
//! Manifest (`plugin.json`):
//!   { "id": "my-plugin", "version": "1.0.0", "description": "...",
//!     "library": "libmy_plugin.so" }   // relative to the manifest's directory

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginInfo {
    pub id: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PluginTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

impl<'de> Deserialize<'de> for PluginTool {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Row {
            name: String,
            description: String,
            input_schema: Value,
        }
        let row = Row::deserialize(deserializer)?;
        Ok(PluginTool {
            name: row.name,
            description: row.description,
            input_schema: row.input_schema,
        })
    }
}

/// The native plugin contract. Implemented by out-of-tree cdylib plugins.
pub trait Plugin: Send + Sync {
    fn info(&self) -> PluginInfo;
    fn tools(&self) -> Vec<PluginTool> {
        Vec::new()
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub id: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    pub library: String,
}

#[derive(Debug)]
pub struct LoadedPlugin {
    pub info: PluginInfo,
    pub tools: Vec<PluginTool>,
    pub manifest_dir: PathBuf,
    // keeps the library mapped; the plugin object points into it
    _library: libloading::Library,
}

#[derive(Debug)]
pub struct Host {
    plugins: Vec<LoadedPlugin>,
}

impl Host {
    pub fn empty() -> Self {
        Host {
            plugins: Vec::new(),
        }
    }

    pub fn list(&self) -> Vec<&LoadedPlugin> {
        self.plugins.iter().collect()
    }

    pub fn tool_specs(&self) -> Vec<PluginTool> {
        self.plugins
            .iter()
            .flat_map(|plugin| plugin.tools.clone())
            .collect()
    }

    /// Read every `*/plugin.json` under `dir`. Malformed manifests are reported and skipped.
    pub fn discover(dir: &Path) -> Result<Vec<PathBuf>, String> {
        let mut manifests = Vec::new();
        let entries = std::fs::read_dir(dir).map_err(|error| {
            format!("failed to read plugin directory {}: {error}", dir.display())
        })?;
        for entry in entries.flatten() {
            let manifest_path = entry.path().join("plugin.json");
            if manifest_path.is_file() {
                manifests.push(manifest_path);
            }
        }
        manifests.sort();
        Ok(manifests)
    }

    /// Load one plugin from its manifest path.
    ///
    /// # Safety
    /// The plugin library is compiled Rust code mapped into this process; it must come from
    /// a trusted source and match this toolchain. Same-toolchain trait objects are not a
    /// stable ABI.
    pub unsafe fn load(&mut self, manifest_path: &Path) -> Result<PluginInfo, String> {
        let raw = std::fs::read_to_string(manifest_path)
            .map_err(|error| format!("failed to read {}: {error}", manifest_path.display()))?;
        let manifest: Manifest = serde_json::from_str(&raw)
            .map_err(|error| format!("invalid {}: {error}", manifest_path.display()))?;
        let dir = manifest_path
            .parent()
            .ok_or_else(|| format!("{} has no parent directory", manifest_path.display()))?
            .to_path_buf();
        if Path::new(&manifest.library).is_absolute() {
            return Err(format!(
                "library path must be relative to the plugin directory"
            ));
        }
        let library_path = dir.join(&manifest.library);
        let library = libloading::Library::new(&library_path)
            .map_err(|error| format!("failed to load {}: {error}", library_path.display()))?;
        let entry: libloading::Symbol<unsafe fn() -> *mut dyn Plugin> =
            library.get(b"devcode_create_plugin").map_err(|error| {
                format!(
                    "{} does not export devcode_create_plugin: {error}",
                    library_path.display()
                )
            })?;
        let pointer = entry();
        if pointer.is_null() {
            return Err(format!(
                "{} returned a null plugin pointer",
                library_path.display()
            ));
        }
        let plugin = Box::from_raw(pointer);
        let info = plugin.info();
        if info.id != manifest.id {
            return Err(format!(
                "plugin id mismatch: manifest says {}, library reports {}",
                manifest.id, info.id
            ));
        }
        let tools = plugin.tools();
        // leak the plugin object; it must outlive the mapped library
        let _leaked: &'static mut dyn Plugin = Box::leak(plugin);
        self.plugins.push(LoadedPlugin {
            info: info.clone(),
            tools,
            manifest_dir: dir,
            _library: library,
        });
        Ok(info)
    }

    /// Discover and load every plugin under `dir`; returns (loaded, errors).
    ///
    /// # Safety
    /// See [`Host::load`].
    pub unsafe fn load_dir(&mut self, dir: &Path) -> (Vec<PluginInfo>, Vec<String>) {
        let mut loaded = Vec::new();
        let mut errors = Vec::new();
        match Host::discover(dir) {
            Ok(manifests) => {
                for manifest in manifests {
                    match self.load(&manifest) {
                        Ok(info) => loaded.push(info),
                        Err(error) => errors.push(error),
                    }
                }
            }
            Err(error) => errors.push(error),
        }
        (loaded, errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_library() -> PathBuf {
        let workspace_target = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("target")
            .join("debug")
            .join("libdevcode_plugin_fixture.so");
        assert!(
            workspace_target.exists(),
            "fixture cdylib missing: {}",
            workspace_target.display()
        );
        workspace_target
    }

    fn plugin_dir(tag: &str) -> PathBuf {
        // layout: <dir>/<plugin>/plugin.json + <dir>/<plugin>/libfixture.so
        let dir =
            std::env::temp_dir().join(format!("devcode-plugin-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let nested = dir.join("fixture");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::copy(fixture_library(), nested.join("libfixture.so")).unwrap();
        nested
    }

    fn write_manifest(dir: &Path, id: &str) {
        std::fs::write(
            dir.join("plugin.json"),
            serde_json::json!({
                "id": id,
                "version": "0.1.0-rust",
                "description": "fixture",
                "library": "libfixture.so",
            })
            .to_string(),
        )
        .unwrap();
    }

    #[test]
    fn loads_fixture_plugin() {
        let dir = plugin_dir("load");
        write_manifest(&dir, "fixture");
        let mut host = Host::empty();
        let (loaded, errors) = unsafe { host.load_dir(dir.parent().unwrap()) };
        assert!(errors.is_empty(), "errors: {errors:?}");
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, "fixture");
        let tools = host.tool_specs();
        assert_eq!(tools[0].name, "fixture_echo");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_manifest_id_mismatch() {
        let dir = plugin_dir("mismatch");
        write_manifest(&dir, "someone-elses-plugin");
        let mut host = Host::empty();
        let (loaded, errors) = unsafe { host.load_dir(dir.parent().unwrap()) };
        assert!(loaded.is_empty());
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("id mismatch"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn skips_directories_without_manifest() {
        let dir = plugin_dir("empty");
        let _unused = &dir; // layout already contains only the plugin dir without a manifest after wipe
        let manifests = Host::discover(dir.parent().unwrap()).unwrap();
        assert!(manifests.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn malformed_manifest_reports_error() {
        let dir = plugin_dir("malformed");
        std::fs::write(dir.join("plugin.json"), "{not json").unwrap();
        let mut host = Host::empty();
        let (loaded, errors) = unsafe { host.load_dir(dir.parent().unwrap()) };
        assert!(loaded.is_empty());
        assert_eq!(errors.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
