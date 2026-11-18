//! `devcode service ...` subcommands. File formats and stop/start semantics mirror the
//! Bun build. One deliberate deviation: a healthy incumbent whose version does not contain
//! "rust" (e.g. the Bun 0.0.x build) is treated as foreign and never killed or replaced
//! unless `--force` is passed to `stop`, so running the JS and Rust services on the same
//! machine cannot destroy each other.

use std::process::{Command, ExitCode, Stdio};
use std::time::Duration;

use serde_json::json;

use devcode_service as svc;

fn registration_path(paths: &devcode_util::Paths) -> std::path::PathBuf {
    paths
        .state
        .join(svc::config_filename(devcode_util::CHANNEL))
}

fn config_path(paths: &devcode_util::Paths) -> std::path::PathBuf {
    svc::config_file(paths, devcode_util::CHANNEL)
}

fn is_rust_version(version: &str) -> bool {
    version.contains("rust")
}

pub fn run(args: &[String]) -> ExitCode {
    let paths = devcode_util::paths("devcode");
    match args.first().map(String::as_str) {
        Some("status") => status(&paths),
        Some("start") => start(&paths),
        Some("stop") => stop_cmd(&paths, args.get(1).map(String::as_str) == Some("--force")),
        Some("restart") => {
            let _ = stop_service(&paths, false);
            start(&paths)
        }
        Some("get") => get_cmd(&paths, &args[1..]),
        Some("set") => set_cmd(&paths, &args[1..]),
        Some("unset") => unset_cmd(&paths, &args[1..]),
        _ => {
            eprintln!("usage: devcode service <status|start|stop|restart|get|set|unset> [args]");
            eprintln!("stop accepts --force to stop a non-rust (Bun) service");
            ExitCode::from(2)
        }
    }
}

fn incumbent(paths: &devcode_util::Paths) -> Option<svc::Registration> {
    svc::Registration::read(&registration_path(paths))
}

fn status(paths: &devcode_util::Paths) -> ExitCode {
    match incumbent(paths) {
        Some(reg) if svc::probe(&reg, 1000) == svc::Health::Ready => {
            println!("{}", reg.url);
            ExitCode::SUCCESS
        }
        _ => {
            println!("stopped");
            ExitCode::SUCCESS
        }
    }
}

fn stop_cmd(paths: &devcode_util::Paths, force: bool) -> ExitCode {
    match stop_service(paths, force) {
        Ok(true) => {
            println!("service stopped");
            ExitCode::SUCCESS
        }
        Ok(false) => {
            println!("no running service");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

/// Returns Ok(true) if a service was stopped, Ok(false) if none was registered.
fn stop_service(paths: &devcode_util::Paths, force: bool) -> Result<bool, String> {
    let Some(reg) = incumbent(paths) else {
        return Ok(false);
    };
    if !is_rust_version(&reg.version) && !force {
        return Err(format!(
            "refusing to stop the {version} service at {url} (not this build). Pass --force to stop it.",
            version = reg.version,
            url = reg.url,
        ));
    }
    terminate(&reg);
    reg.remove_if_owned(&registration_path(paths));
    Ok(true)
}

fn terminate(reg: &svc::Registration) {
    unsafe {
        libc::kill(reg.pid as libc::pid_t, libc::SIGTERM);
    }
    for _ in 0..50 {
        let alive = unsafe { libc::kill(reg.pid as libc::pid_t, 0) == 0 };
        if !alive {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    unsafe {
        libc::kill(reg.pid as libc::pid_t, libc::SIGKILL);
    }
}

fn start(paths: &devcode_util::Paths) -> ExitCode {
    let reg_path = registration_path(paths);
    if let Some(reg) = incumbent(paths) {
        match svc::probe(&reg, 1000) {
            svc::Health::Ready if reg.version == devcode_util::VERSION => {
                println!("{}", reg.url);
                return ExitCode::SUCCESS;
            }
            svc::Health::Ready | svc::Health::Waiting | svc::Health::Failed => {
                if !is_rust_version(&reg.version) {
                    eprintln!(
                        "a {version} service is already registered at {url}. refusing to replace it.\n\
                         run `DEVcode service set port <port>` to start this build on another port",
                        version = reg.version,
                        url = reg.url,
                    );
                    return ExitCode::FAILURE;
                }
                // Rust build version changed underneath us: replace it like the Bun build does.
                terminate(&reg);
                reg.remove_if_owned(&reg_path);
            }
            svc::Health::Incompatible | svc::Health::Unreachable => {
                let _ = std::fs::remove_file(&reg_path);
            }
        }
    }

    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(error) => {
            eprintln!("failed to locate current executable: {error}");
            return ExitCode::FAILURE;
        }
    };
    let log_dir = paths.data.join("log");
    let _ = std::fs::create_dir_all(&log_dir);
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_dir.join("devcode.log"));
    let (log_out, log_err) = match log {
        Ok(file) => (
            Stdio::from(file.try_clone().expect("log clone")),
            Stdio::from(file),
        ),
        Err(_) => (Stdio::null(), Stdio::null()),
    };
    let mut command = Command::new(exe);
    command
        .arg("serve")
        .arg("--service")
        .stdin(Stdio::null())
        .stdout(log_out)
        .stderr(log_err);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            command.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
    if let Err(error) = command.spawn() {
        eprintln!("failed to spawn the managed service: {error}");
        return ExitCode::FAILURE;
    }

    // Poll the registration file and probe; the service writes it before serving.
    for _ in 0..150 {
        std::thread::sleep(Duration::from_millis(100));
        if let Some(reg) = svc::Registration::read(&reg_path) {
            if svc::probe(&reg, 500) == svc::Health::Ready {
                println!("{}", reg.url);
                return ExitCode::SUCCESS;
            }
        }
    }
    eprintln!(
        "service did not become ready within 15s; see {}/log/devcode.log",
        paths.data.display()
    );
    ExitCode::FAILURE
}

fn get_cmd(paths: &devcode_util::Paths, args: &[String]) -> ExitCode {
    let Some(key) = args.first().map(String::as_str) else {
        eprintln!("usage: devcode service get <key> [name]");
        return ExitCode::from(2);
    };
    let path = config_path(paths);
    let mut config = svc::Config::read(&path);
    match key {
        "disabled" => println!("{}", config.get_bool("disabled").unwrap_or(false)),
        "hostname" => println!(
            "{}",
            config
                .get("hostname")
                .and_then(|v| v.as_str())
                .unwrap_or("")
        ),
        "port" => println!(
            "{}",
            config.get_port().map(|p| p.to_string()).unwrap_or_default()
        ),
        "password" => {
            if let Some(existing) = config.get_password() {
                println!("{existing}");
                return ExitCode::SUCCESS;
            }
            let generated = svc::generate_password();
            config.set("password", json!(generated));
            if let Err(error) = config.write(&path) {
                eprintln!("failed to persist service password: {error}");
                return ExitCode::FAILURE;
            }
            println!("{generated}");
        }
        "cors" => println!(
            "{}",
            serde_json::to_string_pretty(&config.get("cors").cloned().unwrap_or_else(|| json!([])))
                .unwrap()
        ),
        "env" => match args.get(1) {
            Some(name) => println!(
                "{}",
                config
                    .get("env")
                    .and_then(|v| v.get(name))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
            ),
            None => println!(
                "{}",
                serde_json::to_string_pretty(
                    &config.get("env").cloned().unwrap_or_else(|| json!({}))
                )
                .unwrap()
            ),
        },
        "remote" => println!("{}", config.get("remote").is_some()),
        _ => {
            eprintln!("unknown service config key: {key}");
            return ExitCode::from(2);
        }
    }
    ExitCode::SUCCESS
}

fn stop_for_config_change(paths: &devcode_util::Paths) {
    let _ = stop_service(paths, false);
}

fn set_cmd(paths: &devcode_util::Paths, args: &[String]) -> ExitCode {
    let (Some(key), Some(value)) = (
        args.first().map(String::as_str),
        args.get(1).map(String::as_str),
    ) else {
        eprintln!("usage: devcode service set <key> <value> [nestedValue]");
        return ExitCode::from(2);
    };
    let path = config_path(paths);
    let mut config = svc::Config::read(&path);
    match key {
        "disabled" => {
            if value != "true" && value != "false" {
                eprintln!("Disabled must be true or false");
                return ExitCode::FAILURE;
            }
            stop_for_config_change(paths);
            config.set("disabled", json!(value == "true"));
        }
        "hostname" => {
            stop_for_config_change(paths);
            config.set("hostname", json!(value));
        }
        "port" => {
            let Ok(port) = value.parse::<u16>() else {
                eprintln!("Port must be between 1 and 65535");
                return ExitCode::FAILURE;
            };
            if port == 0 {
                eprintln!("Port must be between 1 and 65535");
                return ExitCode::FAILURE;
            }
            stop_for_config_change(paths);
            config.set("port", json!(port));
        }
        "password" => {
            stop_for_config_change(paths);
            config.set("password", json!(value));
        }
        "cors" => {
            let origins: Vec<&str> = value
                .split(',')
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .collect();
            for origin in &origins {
                let valid = (origin.starts_with("http://") || origin.starts_with("https://"))
                    && !origin[origin.find("://").unwrap() + 3..].contains('/')
                    && !origin.ends_with('/');
                if !valid {
                    eprintln!("CORS must be a comma-separated list of HTTP(S) origins without paths or trailing slashes");
                    return ExitCode::FAILURE;
                }
            }
            stop_for_config_change(paths);
            config.set("cors", json!(origins));
        }
        "env" => {
            let Some(nested) = args.get(2) else {
                eprintln!("Usage: devcode service set env <key> <value>");
                return ExitCode::from(2);
            };
            stop_for_config_change(paths);
            let mut env = config.get("env").cloned().unwrap_or_else(|| json!({}));
            if !env.is_object() {
                env = json!({});
            }
            env[value] = json!(nested);
            config.set("env", env);
        }
        "remote" => {
            eprintln!("remote access is not supported in the rust build yet");
            return ExitCode::FAILURE;
        }
        _ => {
            eprintln!("Unknown service config key: {key}");
            return ExitCode::from(2);
        }
    }
    if let Err(error) = config.write(&path) {
        eprintln!("failed to write service config: {error}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn unset_cmd(paths: &devcode_util::Paths, args: &[String]) -> ExitCode {
    let Some(key) = args.first().map(String::as_str) else {
        eprintln!("usage: devcode service unset <key> [name]");
        return ExitCode::from(2);
    };
    let path = config_path(paths);
    let mut config = svc::Config::read(&path);
    match key {
        "disabled" | "hostname" | "port" | "password" | "cors" => {
            stop_for_config_change(paths);
            config.unset(key);
        }
        "env" => {
            let Some(name) = args.get(1) else {
                eprintln!("usage: devcode service unset env <key>");
                return ExitCode::from(2);
            };
            stop_for_config_change(paths);
            let mut env = config.get("env").cloned().unwrap_or_else(|| json!({}));
            if let Some(object) = env.as_object_mut() {
                object.remove(name);
                if object.is_empty() {
                    config.unset("env");
                } else {
                    config.set("env", env);
                }
            } else {
                config.unset("env");
            }
        }
        "remote" => {
            eprintln!("remote access is not supported in the rust build yet");
            return ExitCode::FAILURE;
        }
        _ => {
            eprintln!("Unknown service config key: {key}");
            return ExitCode::from(2);
        }
    }
    if let Err(error) = config.write(&path) {
        eprintln!("failed to write service config: {error}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
