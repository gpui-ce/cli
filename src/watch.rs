use crate::{
    Watch,
    compiler::{self, LinkCapture, Process},
    native::Cache,
    server::Server,
};
use anyhow::{Context, Result, ensure};
use cargo_metadata::{Metadata, TargetKind};
use notify::{
    EventKind, RecursiveMode, Watcher,
    event::{AccessKind, AccessMode},
};
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tokio::{process::Command, sync::mpsc};

struct Project {
    manifest: PathBuf,
    root: PathBuf,
    package: String,
    target: String,
    example: bool,
    has_library: bool,
    host: String,
    target_dir: PathBuf,
    roots: Vec<PathBuf>,
    dependency_roots: Vec<PathBuf>,
}

impl Watch {
    fn cargo_options(&self, command: &mut Command) {
        if let Some(path) = &self.manifest_path {
            command.arg("--manifest-path").arg(path);
        }
        let features: Vec<_> = self
            .features
            .iter()
            .flat_map(|s| s.split([',', ' ']))
            .filter(|s| !s.is_empty())
            .collect();
        if !features.is_empty() {
            command.arg("--features").arg(features.join(","));
        }
        if self.all_features {
            command.arg("--all-features");
        }
        if self.no_default_features {
            command.arg("--no-default-features");
        }
        if self.locked {
            command.arg("--locked");
        }
        if self.offline {
            command.arg("--offline");
        }
    }
}

impl Project {
    async fn resolve(options: &Watch) -> Result<Self> {
        crate::platform::Platform::host()?;
        let version = Command::new("rustc").arg("-vV").output().await?;
        ensure!(version.status.success(), "cannot query rustc");
        let version = String::from_utf8(version.stdout)?;
        let host = version
            .lines()
            .find_map(|line| line.strip_prefix("host: "))
            .context("missing rustc host triple")?
            .to_owned();
        ensure!(
            host.starts_with("x86_64-") || host.starts_with("aarch64-"),
            "native watch requires x86_64 or aarch64"
        );
        if let Some(manifest) = &options.manifest_path {
            ensure!(
                manifest.is_file() && manifest.file_name().is_some_and(|n| n == "Cargo.toml"),
                "--manifest-path must point to a Cargo.toml file"
            );
        }
        let mut command = Command::new("cargo");
        command.args([
            "metadata",
            "--format-version",
            "1",
            "--filter-platform",
            &host,
        ]);
        options.cargo_options(&mut command);
        // Metadata stderr is inherited so dependency resolution diagnostics remain visible.
        command.stderr(std::process::Stdio::inherit());
        let output = command.output().await?;
        ensure!(output.status.success(), "cargo metadata failed");
        let metadata: Metadata = serde_json::from_slice(&output.stdout)?;
        let package = if let Some(name) = &options.package {
            let matches: Vec<_> = metadata
                .packages
                .iter()
                .filter(|p| p.name.as_str() == name && metadata.workspace_members.contains(&p.id))
                .collect();
            ensure!(
                matches.len() == 1,
                "package {name:?} is not a unique workspace member"
            );
            matches[0]
        } else {
            metadata
                .root_package()
                .context("select an application package with -p in a virtual workspace")?
        };
        let kind = if options.example.is_some() {
            TargetKind::Example
        } else {
            TargetKind::Bin
        };
        let targets: Vec<_> = package
            .targets
            .iter()
            .filter(|t| t.kind.contains(&kind))
            .collect();
        let requested = options
            .example
            .as_ref()
            .or(options.bin.as_ref())
            .or(package.default_run.as_ref());
        let target = if let Some(name) = requested {
            targets
                .iter()
                .find(|t| t.name == *name)
                .copied()
                .with_context(|| format!("no {kind} target {name:?} in {}", package.name))?
        } else {
            ensure!(
                targets.len() == 1,
                "select a binary with --bin or an example with --example"
            );
            targets[0]
        };
        let resolve = metadata
            .resolve
            .as_ref()
            .context("missing Cargo dependency graph")?;
        let mut reachable = HashSet::new();
        let mut pending = vec![package.id.clone()];
        while let Some(id) = pending.pop() {
            if reachable.insert(id.clone())
                && let Some(node) = resolve.nodes.iter().find(|n| n.id == id)
            {
                pending.extend(node.deps.iter().map(|d| d.pkg.clone()));
            }
        }
        let dependencies: Vec<_> = metadata
            .packages
            .iter()
            .filter(|p| reachable.contains(&p.id))
            .collect();
        ensure!(
            dependencies
                .iter()
                .any(|p| p.name.as_str() == "subsecond" && p.version.to_string() == "0.7.10"),
            "enable GPUI-CE's hot-patching feature (Subsecond 0.7.10 is required)"
        );
        ensure!(
            !dependencies
                .iter()
                .any(|p| p.name.as_str() == "subsecond" && p.version.to_string() != "0.7.10"),
            "multiple or incompatible Subsecond runtime versions are unsupported"
        );
        let root = package
            .manifest_path
            .parent()
            .context("manifest directory missing")?
            .as_std_path()
            .to_owned();
        let mut roots: Vec<_> = dependencies
            .iter()
            .filter(|p| p.source.is_none())
            .filter_map(|p| p.manifest_path.parent())
            .map(|p| p.as_std_path().to_owned())
            .collect();
        let dependency_roots = dependencies
            .iter()
            .filter(|p| p.source.is_none() && p.id != package.id)
            .filter_map(|p| p.manifest_path.parent())
            .map(|p| p.as_std_path().to_owned())
            .collect();
        // A custom binary source can live outside the usual src/examples directories.
        if let Some(parent) = target.src_path.parent() {
            roots.push(parent.as_std_path().to_owned());
        }
        roots.push(metadata.workspace_root.as_std_path().to_owned());
        roots.sort();
        roots.dedup();
        Ok(Self {
            manifest: package.manifest_path.as_std_path().to_owned(),
            root,
            package: package.name.to_string(),
            target: target.name.clone(),
            example: options.example.is_some(),
            has_library: package.targets.iter().any(|t| {
                t.kind.iter().any(|kind| {
                    matches!(
                        kind,
                        TargetKind::Lib
                            | TargetKind::RLib
                            | TargetKind::DyLib
                            | TargetKind::CDyLib
                            | TargetKind::StaticLib
                            | TargetKind::ProcMacro
                    )
                })
            }),
            host,
            target_dir: metadata.target_directory.as_std_path().join("gpui"),
            roots,
            dependency_roots,
        })
    }

    async fn build(&self, options: &Watch, session: &Path) -> Result<PathBuf> {
        // Force a fresh capture on each baseline build without recompiling dependencies.
        if let Ok(bytes) = fs::read(session.join("link.json"))
            && let Ok(link) = serde_json::from_slice::<LinkCapture>(&bytes)
            && let Some(output) = compiler::value(&link.args, "-o")
            && Path::new(output).starts_with(&self.target_dir)
        {
            match fs::remove_file(output) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        let mut command = Command::new("cargo");
        command.arg("rustc");
        options.cargo_options(&mut command);
        if options.manifest_path.is_none() {
            command.arg("--manifest-path").arg(&self.manifest);
        }
        command
            .arg("--package")
            .arg(&self.package)
            .arg(if self.example { "--example" } else { "--bin" })
            .arg(&self.target)
            .args([
                "--profile",
                "gpui-dev",
                "--config",
                "profile.gpui-dev.inherits=\"dev\"",
                "--config",
                "profile.gpui-dev.debug=2",
                "--config",
                "profile.gpui-dev.strip=false",
                "--config",
                "profile.gpui-dev.debug-assertions=true",
                "--config",
                "profile.gpui-dev.opt-level=0",
                "--config",
                "profile.gpui-dev.panic=\"unwind\"",
            ])
            .arg("--target")
            .arg(&self.host)
            .arg("--target-dir")
            .arg(&self.target_dir)
            .args([
                "--",
                "-Csave-temps=true",
                "-Clink-dead-code",
                "-Crelocation-model=pic",
            ])
            .arg(format!("-Clinker={}", session.join("linker").display()))
            .env("RUSTC_WORKSPACE_WRAPPER", session.join("rustc-wrapper"))
            .env("GPUI_DRIVER_SESSION", session)
            .env("GPUI_TARGET_DIR", &self.target_dir)
            .env("GPUI_TIP_CRATE", self.target.replace('-', "_"))
            .env("GPUI_LINK_MODE", "base");
        if let Some(linker) = &options.linker {
            command.env("GPUI_LINKER_OVERRIDE", linker);
        }
        compiler::run(&mut command)
            .await
            .context("baseline build")?;
        let link: LinkCapture = serde_json::from_slice(
            &fs::read(session.join("link.json")).context("missing linker capture")?,
        )?;
        let output = compiler::value(&link.args, "-o").context("missing baseline executable")?;
        let app = session.join(format!(
            "application-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        fs::copy(output, &app).context("copy baseline executable")?;
        Ok(app)
    }
}

type Snapshot = BTreeMap<PathBuf, u64>;

fn source_file(path: &Path) -> bool {
    path.file_name().is_some_and(|n| n == "Cargo.lock")
        || path.extension().is_some_and(|e| {
            matches!(
                e.to_str(),
                Some("rs" | "toml" | "wgsl" | "glsl" | "hlsl" | "metal")
            )
        })
}

fn snapshot(roots: &[PathBuf]) -> Result<Snapshot> {
    let mut snapshot = Snapshot::new();
    for root in roots {
        for entry in walkdir::WalkDir::new(root).into_iter().filter_entry(|e| {
            e.depth() == 0
                || !e.file_type().is_dir()
                || !matches!(
                    e.file_name().to_str(),
                    Some("target" | ".git" | ".direnv" | "node_modules")
                )
        }) {
            let entry = entry?;
            if !entry.file_type().is_file() || !source_file(entry.path()) {
                continue;
            }
            let bytes = match fs::read(entry.path()) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            bytes.hash(&mut hasher);
            snapshot.insert(entry.path().to_owned(), hasher.finish());
        }
    }
    Ok(snapshot)
}

fn changes(old: &Snapshot, new: &Snapshot) -> Vec<PathBuf> {
    old.keys()
        .chain(new.keys())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .filter(|p| old.get(*p) != new.get(*p))
        .cloned()
        .collect()
}

fn valid_sources(paths: &[PathBuf]) -> bool {
    paths
        .iter()
        .filter(|p| p.extension().is_some_and(|e| e == "rs") && p.exists())
        .all(|p| fs::read_to_string(p).is_ok_and(|s| syn::parse_file(&s).is_ok()))
}

fn create_watcher(
    roots: &[PathBuf],
    tx: mpsc::UnboundedSender<()>,
) -> Result<notify::RecommendedWatcher> {
    let mut watcher =
        notify::recommended_watcher(move |event: notify::Result<notify::Event>| match event {
            Ok(event)
                if matches!(
                    event.kind,
                    EventKind::Create(_)
                        | EventKind::Modify(_)
                        | EventKind::Remove(_)
                        | EventKind::Access(AccessKind::Close(AccessMode::Write))
                ) =>
            {
                let _ = tx.send(());
            }
            Err(error) => eprintln!("watcher: {error}"),
            _ => {}
        })?;
    for root in roots {
        watcher.watch(root, RecursiveMode::NonRecursive)?;
        for dir in ["src", "examples", "tests"] {
            let dir = root.join(dir);
            if dir.is_dir() {
                watcher.watch(&dir, RecursiveMode::Recursive)?;
            }
        }
        // Custom source roots without a Cargo manifest are safe to watch recursively.
        if !root.join("Cargo.toml").exists() {
            watcher.watch(root, RecursiveMode::Recursive)?;
        }
    }
    Ok(watcher)
}

pub async fn run(options: Watch) -> Result<()> {
    let mut project = Project::resolve(&options).await?;
    fs::create_dir_all(&project.target_dir)?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(project.target_dir.join("watch.lock"))?;
    fs2::FileExt::try_lock_exclusive(&lock)
        .context("another gpui watch is using this target directory")?;
    let session = project.target_dir.join("sessions").join(format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    fs::create_dir_all(&session)?;
    // Captures contain the compiler environment, so keep them private.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&session, fs::Permissions::from_mode(0o700))?;
    }
    compiler::scripts(&session)?;
    let server = Server::start(options.port).await?;
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut watcher = create_watcher(&project.roots, tx.clone())?;
    let mut last = snapshot(&project.roots)?;
    let mut generation = 1;
    let mut sequence = 0;
    let mut pending = std::collections::BTreeSet::new();
    let mut baseline_pending = false;
    eprintln!(
        "Building {} with the native Subsecond driver",
        project.target
    );
    let executable = project.build(&options, &session).await?;
    let mut cache = Cache::new(&executable)?;
    let mut app = launch(&options, &project, &executable, &server, generation)?;
    let mut connection = server.connection().await?;
    eprintln!(
        "Watching {} (PID {}, patch server 127.0.0.1:{})",
        project.target,
        app.id(),
        server.port
    );
    let debounce = Duration::from_millis(options.debounce_ms);
    loop {
        tokio::select! {
            status = app.wait() => {
                let status = status?;
                ensure!(status.success(), "application exited with {status}");
                return Ok(());
            }
            _ = rx.recv() => {},
            _ = tokio::time::sleep(Duration::from_secs(1)) => {},
        }
        // Polling provides a fallback for dropped filesystem events. Require two
        // matching snapshots so incomplete saves are not compiled immediately.
        tokio::time::sleep(debounce).await;
        while rx.try_recv().is_ok() {}
        let current = snapshot(&project.roots)?;
        let changed = changes(&last, &current);
        if changed.is_empty() {
            continue;
        }
        tokio::time::sleep(debounce).await;
        if current != snapshot(&project.roots)? {
            continue;
        }
        last = current;
        pending.extend(changed);
        let changed: Vec<_> = pending.iter().cloned().collect();
        if !valid_sources(&changed) {
            eprintln!("Waiting for valid Rust source; the running application is unchanged");
            continue;
        }
        let restart = baseline_pending
            || project.has_library
            || changed.iter().any(|p| {
                !p.starts_with(&project.root)
                    || project
                        .dependency_roots
                        .iter()
                        .any(|root| p.starts_with(root))
                    || p.extension().is_none_or(|e| e != "rs")
                    || p.file_name().is_some_and(|n| n == "build.rs")
            });
        if restart {
            eprintln!("Dependency or build configuration changed; rebuilding and restarting");
            baseline_pending = true;
            let rebuilt = async {
                let next = Project::resolve(&options).await?;
                ensure!(
                    next.target_dir == project.target_dir,
                    "target directory changed; stop watch and run it again"
                );
                let next_watcher = create_watcher(&next.roots, tx.clone())?;
                let executable = next.build(&options, &session).await?;
                let cache = Cache::new(&executable)?;
                Ok::<_, anyhow::Error>((next, next_watcher, executable, cache))
            }
            .await;
            match rebuilt {
                Ok((next, next_watcher, executable, next_cache)) => {
                    drop(app);
                    project = next;
                    watcher = next_watcher;
                    generation += 1;
                    cache = next_cache;
                    app = launch(&options, &project, &executable, &server, generation)?;
                    connection = server.connection().await?;
                    pending.clear();
                    baseline_pending = false;
                    eprintln!("Restarted {} (PID {})", project.target, app.id());
                }
                Err(error) => {
                    eprintln!("Rebuild failed: {error:#}. Fix the source and save to retry.")
                }
            }
        } else {
            sequence += 1;
            let start = Instant::now();
            match compiler::patch(&session, &cache, connection.aslr_reference, sequence)
                .await
                .and_then(|path| cache.jump_table(&path))
            {
                Ok(table) => {
                    pending.clear();
                    server.patch(table, start.elapsed().as_millis() as u64)?;
                    eprintln!(
                        "Hot-patched {} in {:.2}s (PID {})",
                        project.target,
                        start.elapsed().as_secs_f64(),
                        app.id()
                    );
                }
                Err(error) => {
                    eprintln!("Patch failed: {error:#}. Fix the source and save to retry.")
                }
            }
        }
        // Keep the OS watcher alive across await points and replace it on rebuild.
        let _ = &watcher;
    }
}

fn launch(
    options: &Watch,
    project: &Project,
    executable: &Path,
    server: &Server,
    generation: u64,
) -> Result<Process> {
    let args = if let Some(args) = &options.args {
        shell_words::split(args)?
            .into_iter()
            .map(Into::into)
            .collect()
    } else {
        options.application_args.clone()
    };
    let mut command = Command::new(executable);
    command
        .args(args)
        .current_dir(&project.root)
        .env("DIOXUS_DEVSERVER_IP", "127.0.0.1")
        .env("DIOXUS_DEVSERVER_PORT", server.port.to_string())
        .env("DIOXUS_BUILD_ID", generation.to_string())
        .env("DIOXUS_CLI_ENABLED", "true");
    let process = Process::spawn(&mut command)?;
    server.expect(process.id(), generation);
    Ok(process)
}
