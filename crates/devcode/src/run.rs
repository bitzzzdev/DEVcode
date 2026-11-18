//! Headless CLI entry: `DEVcode run "<prompt>"`. Parses flags, then delegates to
//! devcode-runner (shared with the HTTP prompt endpoint).

use std::io::Write as _;
use std::process::ExitCode;

pub struct RunOptions {
    pub prompt: String,
    pub model: devcode_ai::ModelRef,
    pub session_id: Option<String>,
    pub workdir: std::path::PathBuf,
    pub max_steps: u32,
}

pub fn parse_args(args: &[String]) -> Result<RunOptions, String> {
    let mut prompt_parts: Vec<String> = Vec::new();
    let mut model: Option<String> = std::env::var("DEVCODE_MODEL")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let mut session_id: Option<String> = None;
    let mut workdir: Option<std::path::PathBuf> = None;
    let mut max_steps: u32 = 32;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--model" => {
                index += 1;
                model = Some(args.get(index).cloned().ok_or("--model requires a value")?);
            }
            "--session" => {
                index += 1;
                session_id = Some(
                    args.get(index)
                        .cloned()
                        .ok_or("--session requires a value")?,
                );
            }
            "--workdir" => {
                index += 1;
                workdir = Some(std::path::PathBuf::from(
                    args.get(index)
                        .cloned()
                        .ok_or("--workdir requires a value")?,
                ));
            }
            "--max-steps" => {
                index += 1;
                max_steps = args
                    .get(index)
                    .and_then(|value| value.parse::<u32>().ok())
                    .ok_or("--max-steps requires a positive integer")?;
            }
            "--help" | "-h" => return Err(help()),
            other => prompt_parts.push(other.to_owned()),
        }
        index += 1;
    }
    let prompt = prompt_parts.join(" ").trim().to_owned();
    if prompt.is_empty() {
        return Err(format!("a prompt is required\n{}", help()));
    }
    let model = model.ok_or_else(|| {
        format!(
            "no model selected; pass --model provider/model[#variant] or set DEVCODE_MODEL\n{}",
            help()
        )
    })?;
    let model = devcode_ai::parse_model(&model)?;
    Ok(RunOptions {
        prompt,
        model,
        session_id,
        workdir: workdir.unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into())),
        max_steps,
    })
}

fn help() -> String {
    "usage: DEVcode run [--model provider/model[#variant]] [--session ID] [--workdir PATH] [--max-steps N] <prompt>".to_owned()
}

pub fn run(args: &[String]) -> ExitCode {
    let options = match parse_args(args) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(2);
        }
    };
    let paths = devcode_util::paths("devcode");
    let mut on_text = |delta: &str| {
        print!("{delta}");
        let _ = std::io::stdout().flush();
    };
    let mut on_tool = |name: &str, line: &str| {
        eprintln!("[tool] {name} {line}");
    };
    let execution = devcode_runner::Execution {
        prompt: options.prompt,
        model: options.model,
        session_id: options.session_id,
        directory: options.workdir,
        max_steps: options.max_steps,
        delivery: "steer".to_owned(),
    };
    match devcode_runner::execute(&execution, &paths, &mut on_text, &mut on_tool) {
        Ok(session_id) => {
            println!();
            let _ = std::io::stderr().flush();
            eprintln!("session: {session_id}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            let _ = std::io::stdout().flush();
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}
