//! Fixture plugin: greets with a static name. Used by devcode-plugin tests and by the
//! smoke test for dynamic loading.

use devcode_plugin::{Plugin, PluginInfo};

pub struct Fixture;

impl Plugin for Fixture {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            id: "fixture".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            description: "test fixture plugin".into(),
        }
    }

    fn tools(&self) -> Vec<devcode_plugin::PluginTool> {
        vec![devcode_plugin::PluginTool {
            name: "fixture_echo".into(),
            description: "echoes its input".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": { "text": { "type": "string" } },
                "required": ["text"],
            }),
        }]
    }
}

// Same-toolchain Rust plugin trade-off: trait objects are not a stable C ABI.
#[allow(improper_ctypes_definitions)]
#[no_mangle]
pub extern "C" fn devcode_create_plugin() -> *mut dyn Plugin {
    Box::into_raw(Box::new(Fixture))
}
