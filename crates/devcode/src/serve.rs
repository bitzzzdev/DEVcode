//! Runs the HTTP service: /api/info and a minimal /openapi.json, behind the same
//! basic-auth contract the Bun build uses. Everything else is out of scope until the
//! rest of the product ports over.

use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use base64::Engine;
use devcode_service as svc;
use serde_json::json;
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};

pub fn run(args: &[String]) -> ExitCode {
    let service_mode = args.iter().any(|arg| arg == "--service");
    let paths = devcode_util::paths("devcode");
    let config_path = svc::config_file(&paths, devcode_util::CHANNEL);
    let mut config = svc::Config::read(&config_path);
    let hostname = config
        .get("hostname")
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| svc::DEFAULT_HOSTNAME.to_owned());
    let port = config
        .get_port()
        .unwrap_or_else(|| svc::default_port(devcode_util::CHANNEL));

    // The Bun build persists a generated password; do the same so `service get password` stays stable.
    let password = match std::env::var_os("DEVCODE_PASSWORD")
        .or_else(|| std::env::var_os("DEVCODE_SERVER_PASSWORD"))
    {
        Some(env) => env.to_string_lossy().into_owned(),
        None => match config.get_password() {
            Some(existing) => existing,
            None => {
                let generated = svc::generate_password();
                config.set("password", json!(generated));
                if let Err(error) = config.write(&config_path) {
                    eprintln!("failed to persist generated service password: {error}");
                    return ExitCode::FAILURE;
                }
                generated
            }
        },
    };

    let url = svc::service_url(&hostname, port);
    let registration = svc::Registration {
        id: svc::generate_id(),
        version: devcode_util::VERSION.to_owned(),
        url: url.clone(),
        pid: std::process::id() as i64,
        password: password.clone(),
    };

    // The Bun build opens (and bootstraps) the SQLite database at startup; do the same so
    // the managed service owns its storage file from the first connection.
    let database_path = devcode_core::database_path(&paths);
    if let Err(error) = devcode_core::Database::open(&database_path) {
        eprintln!(
            "failed to open database {}: {error}",
            database_path.display()
        );
        return ExitCode::FAILURE;
    }

    let addr = format!("{hostname}:{port}");
    let server = match Server::http(&addr) {
        Ok(server) => server,
        Err(_) => {
            eprintln!(
                "Managed service port {port} on {hostname} is already in use by another process."
            );
            eprintln!("Configure another port with `devcode service set port <port>` and start the service again.");
            return ExitCode::FAILURE;
        }
    };

    let registration_path = paths.state.join("service.json");
    if service_mode {
        if let Err(error) = registration.write(&registration_path) {
            eprintln!(
                "failed to write service registration {}: {error}",
                registration_path.display()
            );
            return ExitCode::FAILURE;
        }
    }

    let pid = std::process::id();
    let hostname_owned =
        std::fs::read_to_string("/proc/sys/kernel/hostname").unwrap_or_else(|_| "localhost".into());
    let info = json!({
        "version": devcode_util::VERSION,
        "pid": pid,
        "hostname": hostname_owned.trim(),
        "urls": [url],
        "paths": { "tmp": paths.tmp.to_string_lossy() },
        "capabilities": { "persistentPty": true },
    });

    let registered = Arc::new(Mutex::new(Some(registration.clone())));
    let shutdown = registered.clone();
    let registration_path_for_signal = registration_path.clone();
    ctrlc::set_handler(move || {
        let registration = shutdown.lock().unwrap().take();
        if let Some(registration) = registration {
            registration.remove_if_owned(&registration_path_for_signal);
        }
        std::process::exit(0);
    })
    .expect("failed to install signal handler");

    println!("server listening on {url}");
    if !service_mode {
        println!("server password {password}");
    }

    for request in server.incoming_requests() {
        handle(request, &info, &password);
    }
    ExitCode::SUCCESS
}

fn handle(request: Request, info: &serde_json::Value, password: &str) {
    if !authorized(&request, password) {
        let body = json!({"_tag": "UnauthorizedError", "message": "Authentication required"});
        respond(request, 401, body);
        return;
    }
    match (
        request.method(),
        request.url().split('?').next().unwrap_or(""),
    ) {
        (&Method::Get, "/api/info") => respond(request, 200, info.clone()),
        (&Method::Get, "/openapi.json") => respond(request, 200, openapi()),
        _ => respond(request, 404, json!({"message": "Not Found"})),
    }
}

fn respond(request: Request, status: u16, body: serde_json::Value) {
    let payload = serde_json::to_string(&body).unwrap_or_else(|_| "{}".to_string());
    let response = Response::from_string(payload)
        .with_status_code(StatusCode(status))
        .with_header(Header::from_bytes("content-type", "application/json").unwrap());
    let _ = request.respond(response);
}

fn authorized(request: &Request, password: &str) -> bool {
    let Some(header) = request
        .headers()
        .iter()
        .find(|header| header.field.equiv("authorization"))
    else {
        return false;
    };
    let Ok(value) = std::str::from_utf8(header.value.as_bytes()) else {
        return false;
    };
    let Some(encoded) = value.strip_prefix("Basic ") else {
        return false;
    };
    let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(encoded.trim()) else {
        return false;
    };
    let Ok(credentials) = std::str::from_utf8(&decoded) else {
        return false;
    };
    let Some((user, pass)) = credentials.split_once(':') else {
        return false;
    };
    user == svc::AUTH_USERNAME && svc::constant_time_eq(pass, password)
}

fn openapi() -> serde_json::Value {
    json!({
        "openapi": "3.1.0",
        "info": {
            "title": "devcode HttpApi",
            "version": devcode_util::VERSION,
            "description": "Rust port of the DEVcode HttpApi surface. Only /api/info is served so far.",
        },
        "paths": {
            "/api/info": {
                "get": {
                    "tags": ["server"],
                    "summary": "Service information",
                    "responses": { "200": { "description": "Service information" } },
                },
            },
        },
    })
}
