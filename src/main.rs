mod compiler;
mod native;
mod platform;
mod scaffold;
mod server;
mod watch;

use clap::{Parser, Subcommand};
use std::{ffi::OsString, path::PathBuf, process::ExitCode};

#[derive(Parser)]
#[command(name = "gpui", version, about = "Develop GPUI-CE applications")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Create a GPUI-CE counter app with hot-patching enabled
    New(scaffold::New),
    /// Build, run, and hot-patch a native debug application using Subsecond
    Watch(Watch),
}

#[derive(clap::Args, Debug)]
pub struct Watch {
    /// Cargo manifest for the application (defaults to Cargo's current project)
    #[arg(long, value_name = "PATH")]
    manifest_path: Option<PathBuf>,
    /// Package to run in a workspace
    #[arg(short, long)]
    package: Option<String>,
    /// Binary to run
    #[arg(long, conflicts_with = "example")]
    bin: Option<String>,
    /// Example to run
    #[arg(long)]
    example: Option<String>,
    /// Features to enable (comma or space separated; may be repeated)
    #[arg(short = 'F', long)]
    features: Vec<String>,
    #[arg(long)]
    all_features: bool,
    #[arg(long)]
    no_default_features: bool,
    #[arg(long)]
    locked: bool,
    #[arg(long)]
    offline: bool,
    /// Application arguments as a shell-style string
    #[arg(long, allow_hyphen_values = true, conflicts_with = "application_args")]
    args: Option<String>,
    /// C compiler driver used for linking (defaults to Cargo's linker or cc)
    #[arg(long)]
    linker: Option<PathBuf>,
    /// Local patch-server port (defaults to an available port)
    #[arg(long, default_value_t = 0)]
    port: u16,
    /// Wait for source writes to settle before rebuilding
    #[arg(long, default_value_t = 200, value_parser = clap::value_parser!(u64).range(1..))]
    debounce_ms: u64,
    /// Arguments passed directly to the application after --
    #[arg(last = true)]
    application_args: Vec<OsString>,
}

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().collect();
    if args
        .get(1)
        .is_some_and(|arg| arg == "__rustc" || arg == "__link")
    {
        return match compiler::internal(&args[1], &args[2..]) {
            Ok(code) => ExitCode::from(code),
            Err(error) => {
                eprintln!("gpui compiler: {error:#}");
                ExitCode::FAILURE
            }
        };
    }
    let cli = Cli::parse();
    match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime.block_on(run(cli)),
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> ExitCode {
    let result = match cli.command {
        Commands::New(options) => scaffold::run(options),
        Commands::Watch(options) => tokio::select! {
            result = watch::run(options) => result,
            result = shutdown() => result,
        },
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

async fn shutdown() -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result?,
            _ = terminate.recv() => {},
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await?;
    Ok(())
}
