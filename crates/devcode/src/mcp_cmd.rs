//! `DEVcode mcp auth <server-name>` — run the OAuth 2.0 authorization flow for a remote MCP server.

use std::net::TcpListener;

use devcode_mcp::oauth::{self, CredentialStore};
use devcode_mcp::{load_config, McpConfig, ServerConfig};
use ureq::AgentBuilder;

pub fn run(args: &[String]) -> std::process::ExitCode {
    if args.is_empty() || args[0] != "auth" {
        eprintln!("usage: DEVcode mcp auth <server-name>");
        return std::process::ExitCode::from(2);
    }
    let server_name = match args.get(1) {
        Some(name) => name,
        None => {
            eprintln!("missing server name");
            eprintln!("usage: DEVcode mcp auth <server-name>");
            return std::process::ExitCode::from(2);
        }
    };

    let paths = devcode_util::paths("devcode");
    let config_file = paths.config.join("devcode.jsonc");

    let config: McpConfig = match load_config(&config_file) {
        Ok(c) => c,
        Err(error) => {
            eprintln!("failed to read {}: {error}", config_file.display());
            return std::process::ExitCode::from(1);
        }
    };

    // Plugin-contributed servers are visible to auth too, so an installed plugin
    // (e.g. devcode.figma) can be signed in without hand-writing devcode.jsonc.
    // User config still wins on name conflict.
    let plugins = devcode_runner::load_plugin_host(&paths);
    let servers = devcode_runner::merge_plugin_mcp_servers(config.servers, &plugins);

    let server: &ServerConfig = match servers.get(server_name) {
        Some(s) => s,
        None => {
            eprintln!("MCP server not found: {server_name}");
            eprintln!(
                "configured servers: {}",
                servers.keys().cloned().collect::<Vec<_>>().join(", ")
            );
            return std::process::ExitCode::from(1);
        }
    };

    let remote = match server {
        ServerConfig::Remote(remote) => remote,
        ServerConfig::Local(_) => {
            eprintln!("server \"{server_name}\" is not a remote server (type=local)");
            return std::process::ExitCode::from(1);
        }
    };
    if remote.disabled {
        eprintln!("server \"{server_name}\" is disabled");
        return std::process::ExitCode::from(1);
    }

    let db_path = devcode_core::database_path(&paths);
    let store = match CredentialStore::open(&db_path.to_string_lossy()) {
        Ok(store) => store,
        Err(error) => {
            eprintln!("failed to open credential store at {}: {error}", db_path.display());
            return std::process::ExitCode::from(1);
        }
    };

    let integration_id = CredentialStore::integration_id(server_name, &remote.url);
    if let Ok(Some(stored)) = store.get(&integration_id) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        if stored.expires == 0 || stored.expires > now {
            println!("server \"{server_name}\" already has valid credentials");
            return std::process::ExitCode::SUCCESS;
        }
        println!("server \"{server_name}\" has expired credentials, re-authenticating...");
    }

    let port = free_port().unwrap_or(49390);
    let agent = AgentBuilder::new().build();
    match oauth::login(&agent, server_name, &remote.url, None, port) {
        Ok(stored) => match store.save(&integration_id, "default", &stored) {
            Ok(()) => {
                println!("Authorization complete. Credentials saved.");
                std::process::ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("failed to save credentials: {error}");
                std::process::ExitCode::from(1)
            }
        },
        Err(error) => {
            eprintln!("Authorization failed: {error}");
            std::process::ExitCode::from(1)
        }
    }
}

fn free_port() -> Option<u16> {
    TcpListener::bind("127.0.0.1:0")
        .ok()?
        .local_addr()
        .ok()
        .map(|addr| addr.port())
}
