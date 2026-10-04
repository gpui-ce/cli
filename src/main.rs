use clap::{Parser, Subcommand};
use std::{
    ffi::OsString,
    path::PathBuf,
    process::{Command, ExitCode},
};

const DX_VERSION: &str = "0.7.10";

#[derive(Parser)]
#[command(name = "gpui", version, about = "Develop GPUI-CE applications")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Build, run, and hot-patch a native debug application using Subsecond
    Watch(Watch),
}

#[derive(clap::Args)]
struct Watch {
    /// Cargo manifest for the application (defaults to the current directory)
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
    #[arg(short = 'F', long, action = clap::ArgAction::Append)]
    features: Vec<String>,
    #[arg(long)]
    all_features: bool,
    #[arg(long)]
    no_default_features: bool,
    /// Require Cargo.lock to remain unchanged
    #[arg(long)]
    locked: bool,
    /// Avoid network access during the build
    #[arg(long)]
    offline: bool,
    /// Application arguments, using the backend's argument-string syntax
    #[arg(long, allow_hyphen_values = true)]
    args: Option<String>,
    /// Dioxus CLI executable providing the Subsecond compiler
    #[arg(long, env = "GPUI_DX", default_value = "dx", value_name = "PATH")]
    dx: PathBuf,
}

impl Watch {
    fn command(&self) -> Result<Command, String> {
        // Resolve before changing directories so relative backend paths retain their meaning.
        let dx = if self.dx.components().count() > 1 && self.dx.is_relative() {
            std::env::current_dir()
                .map_err(|e| e.to_string())?
                .join(&self.dx)
        } else {
            self.dx.clone()
        };
        let mut command = Command::new(dx);
        command.args(["serve", "--hot-patch", "--watch=true", "--open=false"]);
        if let Some(manifest) = &self.manifest_path {
            let manifest = manifest
                .canonicalize()
                .map_err(|e| format!("cannot open manifest {}: {e}", manifest.display()))?;
            if !manifest.is_file() || manifest.file_name().is_none_or(|name| name != "Cargo.toml") {
                return Err("--manifest-path must point to a Cargo.toml file".into());
            }
            command.current_dir(manifest.parent().unwrap());
        }
        for (flag, value) in [
            ("--package", &self.package),
            ("--bin", &self.bin),
            ("--example", &self.example),
            ("--args", &self.args),
        ] {
            if let Some(value) = value {
                command.arg(flag).arg(value);
            }
        }
        if !self.features.is_empty() {
            command.arg("--features").arg(self.features.join(" "));
        }
        if self.all_features {
            command.arg("--all-features");
        }
        if self.no_default_features {
            command.arg("--no-default-features");
        }
        let mut cargo_args = Vec::new();
        if self.locked {
            cargo_args.push("--locked");
        }
        if self.offline {
            cargo_args.push("--offline");
        }
        if !cargo_args.is_empty() {
            command.arg(format!("--cargo-args={}", cargo_args.join(" ")));
        }
        Ok(command)
    }

    fn run(&self) -> Result<ExitCode, String> {
        let mut command = self.command()?;
        let program: OsString = command.get_program().into();
        let version = Command::new(&program).arg("--version").output().map_err(|e| {
            format!("cannot run {}: {e}\nInstall the Subsecond backend with: cargo install dioxus-cli --version {DX_VERSION} --locked", self.dx.display())
        })?;
        let version_text = String::from_utf8_lossy(&version.stdout);
        if !version.status.success() || !version_text.split_whitespace().any(|v| v == DX_VERSION) {
            return Err(format!(
                "gpui watch requires dioxus-cli {DX_VERSION} to match GPUI-CE's runtime; {} reported {:?}",
                self.dx.display(),
                version_text.trim()
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // Replace gpui with the driver, preserving terminal I/O, signals, and exit status.
            Err(format!("cannot start Subsecond driver: {}", command.exec()))
        }
        #[cfg(not(unix))]
        {
            let status = command
                .status()
                .map_err(|e| format!("cannot start Subsecond driver: {e}"))?;
            Ok(ExitCode::from(status.code().unwrap_or(1) as u8))
        }
    }
}

fn main() -> ExitCode {
    let result = match Cli::parse().command {
        Commands::Watch(watch) => watch.run(),
    };
    match result {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}
