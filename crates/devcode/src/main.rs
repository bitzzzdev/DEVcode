//! DEVcode (Rust build). Ground-up port of packages/cli, starting with the managed
//! background service, the discovery contract, and /api/info. Everything else is
//! intentionally not here yet; see .progress.md for the port ledger.

mod run;
mod serve;
mod service_cmd;

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--version") | Some("-v") | Some("version") => {
            println!("{}", devcode_util::VERSION);
            ExitCode::SUCCESS
        }
        Some("--help") | Some("-h") | Some("help") => {
            print_help();
            ExitCode::SUCCESS
        }
        Some("serve") => serve::run(&args[1..]),
        Some("run") => run::run(&args[1..]),
        Some("service") => service_cmd::run(&args[1..]),
        Some(other) => {
            eprintln!("unknown command: {other}");
            eprintln!("the rust build currently supports: serve, service, run, --version");
            eprintln!(
                "use the bun build (DEVcode 0.0.1, all commands/features) until the port lands"
            );
            ExitCode::from(2)
        }
        None => {
            print_help();
            ExitCode::SUCCESS
        }
    }
}

fn print_help() {
    println!("DEVcode (rust build {})", devcode_util::VERSION);
    println!();
    println!("USAGE");
    println!("  DEVcode serve [--service]");
    println!("  DEVcode service <status|start|stop|restart|get|set|unset> ...");
    println!("  DEVcode run [--model provider/model[#variant]] [--session ID] [--workdir PATH] [--max-steps N] <prompt>");
    println!("  DEVcode --version");
}
