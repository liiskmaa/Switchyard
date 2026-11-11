//! `switchyard-core` executable.

use std::path::PathBuf;
use std::process::ExitCode;

use switchyard_core::logging::{self, LogFormat};
use switchyard_core::{CORE_VERSION, CoreConfig, CoreError};
use tokio::signal::unix::{SignalKind, signal};

const USAGE: &str = "\
Usage: switchyard-core [OPTIONS]

Runs Switchyard Core, the local orchestration process.

Options:
  --data-dir <PATH>     Data directory (default: SWITCHYARD_DATA_DIR or the platform default)
  --socket <PATH>       Socket path (default: SWITCHYARD_SOCKET or <data-dir>/core.sock)
  --log-format <FMT>    text (default) or json
  --version             Print version and exit
  -h, --help            Print this help

Environment:
  SWITCHYARD_LOG        Log filter, e.g. info or switchyard_core=debug
  SWITCHYARD_GIT        git executable (default: git on PATH)

Exit codes: 0 clean shutdown, 1 runtime error, 2 usage/configuration error,
            3 another Core already owns the data directory.";

struct Args {
    data_dir: Option<PathBuf>,
    socket: Option<PathBuf>,
    log_format: LogFormat,
}

enum Parsed {
    Run(Args),
    Exit(ExitCode),
}

fn parse_args() -> Result<Parsed, String> {
    let mut args = Args { data_dir: None, socket: None, log_format: LogFormat::Text };
    let mut it = std::env::args_os().skip(1);
    while let Some(arg) = it.next() {
        let arg = arg.to_str().ok_or("arguments must be valid UTF-8")?.to_owned();
        let mut value = |name: &str| it.next().map(PathBuf::from).ok_or(format!("{name} requires a value"));
        match arg.as_str() {
            "--data-dir" => args.data_dir = Some(value("--data-dir")?),
            "--socket" => args.socket = Some(value("--socket")?),
            "--log-format" => {
                args.log_format = match value("--log-format")?.to_str() {
                    Some("text") => LogFormat::Text,
                    Some("json") => LogFormat::Json,
                    _ => return Err("--log-format must be text or json".into()),
                }
            }
            "--version" => {
                println!(
                    "switchyard-core {CORE_VERSION} (protocol {})",
                    switchyard_protocol::PROTOCOL_VERSION
                );
                return Ok(Parsed::Exit(ExitCode::SUCCESS));
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(Parsed::Exit(ExitCode::SUCCESS));
            }
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    for (flag, path) in [("--data-dir", &args.data_dir), ("--socket", &args.socket)] {
        if path.as_ref().is_some_and(|p| !p.is_absolute()) {
            return Err(format!("{flag} must be an absolute path"));
        }
    }
    Ok(Parsed::Run(args))
}

fn build_config(args: &Args) -> Result<CoreConfig, CoreError> {
    let mut config = CoreConfig::from_env()?;
    if args.data_dir.is_some() || args.socket.is_some() {
        let data_dir = args.data_dir.clone().unwrap_or_else(|| config.paths.data_dir.clone());
        let socket = args.socket.clone().or_else(|| {
            // Keep an env-provided socket unless the data dir moved.
            (args.data_dir.is_none()).then(|| config.paths.socket.clone())
        });
        let git = config.git.clone();
        config = CoreConfig::for_data_dir(data_dir, socket)?;
        config.git = git;
    }
    Ok(config)
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(Parsed::Run(args)) => args,
        Ok(Parsed::Exit(code)) => return code,
        Err(message) => {
            eprintln!("switchyard-core: {message}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    logging::init(args.log_format);

    match run(&args).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            tracing::error!(error = %err, "Switchyard Core failed");
            ExitCode::from(err.exit_code())
        }
    }
}

async fn run(args: &Args) -> Result<(), CoreError> {
    let config = build_config(args)?;
    let core = switchyard_core::start(config).await?;

    let signal_err = |e: std::io::Error| CoreError::Task(format!("cannot install signal handler: {e}"));
    let mut sigterm = signal(SignalKind::terminate()).map_err(signal_err)?;
    let mut sigint = signal(SignalKind::interrupt()).map_err(signal_err)?;
    let mut sighup = signal(SignalKind::hangup()).map_err(signal_err)?;

    tokio::select! {
        _ = sigterm.recv() => core.shutdown("SIGTERM"),
        _ = sigint.recv() => core.shutdown("SIGINT"),
        _ = sighup.recv() => core.shutdown("SIGHUP"),
        _ = core.shutdown_requested() => {}
    }
    core.wait().await
}
