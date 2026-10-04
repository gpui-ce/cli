//! Cargo/rustc capture and native linking. Fat archive and thin-link argument
//! handling adapted from Dioxus CLI build/link.rs at 57d6794 (Apache-2.0).
//! Modified to run entirely inside gpui and support ELF and Mach-O. See NOTICE.

use crate::platform::Platform;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    ffi::{OsStr, OsString},
    fs,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Serialize, Deserialize)]
pub struct RustcCapture {
    pub compiler: PathBuf,
    pub args: Vec<OsString>,
    pub env: Vec<(OsString, OsString)>,
    pub cwd: PathBuf,
}

#[derive(Serialize, Deserialize)]
pub struct LinkCapture {
    pub args: Vec<String>,
    pub linker: PathBuf,
}

pub fn internal(mode: &OsStr, args: &[OsString]) -> Result<u8> {
    let session = PathBuf::from(
        std::env::var_os("GPUI_DRIVER_SESSION")
            .context("internal compiler command requires a watch session")?,
    );
    if mode == "__rustc" {
        let (compiler, args) = args.split_first().context("missing rustc executable")?;
        let mut cmd = Command::new(compiler);
        cmd.args(args);
        let bin = args
            .windows(2)
            .any(|a| a[0] == "--crate-type" && a[1] == "bin");
        let tip = std::env::var("GPUI_TIP_CRATE")?;
        let selected = args
            .windows(2)
            .any(|a| a[0] == "--crate-name" && a[1] == tip.as_str());
        if bin && selected {
            let capture = RustcCapture {
                compiler: compiler.into(),
                args: args.to_vec(),
                env: std::env::vars_os().collect(),
                cwd: std::env::current_dir()?,
            };
            fs::write(session.join("rustc.json"), serde_json::to_vec(&capture)?)?;
            let linker = real_linker(args, &session)?;
            cmd.env("GPUI_REAL_LINKER", linker);
        }
        return Ok(cmd.status()?.code().unwrap_or(1) as u8);
    }
    let args: Vec<String> = args
        .iter()
        .map(|s| {
            s.to_str()
                .context("non-UTF-8 linker argument")
                .map(str::to_owned)
        })
        .collect::<Result<_>>()?;
    let linker =
        PathBuf::from(std::env::var_os("GPUI_REAL_LINKER").context("missing original linker")?);
    fs::write(
        session.join("link.json"),
        serde_json::to_vec(&LinkCapture {
            args: args.clone(),
            linker: linker.clone(),
        })?,
    )?;
    if std::env::var("GPUI_LINK_MODE")?.as_str() == "thin" {
        let output = value(&args, "-o").context("linker output missing")?;
        fs::write(output, [])?;
        return Ok(0);
    }
    let target =
        PathBuf::from(std::env::var_os("GPUI_TARGET_DIR").context("missing target directory")?);
    let archive = session.join("dependencies.a");
    let args = fat_link_args(&args, &target, &archive, Platform::host()?)?;
    let status = Command::new(linker).args(args).status()?;
    Ok(status.code().unwrap_or(1) as u8)
}

fn real_linker(args: &[OsString], session: &Path) -> Result<PathBuf> {
    if let Some(linker) = std::env::var_os("GPUI_LINKER_OVERRIDE") {
        return Ok(linker.into());
    }
    let own = format!("linker={}", session.join("linker").display());
    let mut selected = PathBuf::from("cc");
    for (i, arg) in args.iter().enumerate() {
        let text = arg.to_string_lossy();
        let option = text
            .strip_prefix("-C")
            .filter(|s| !s.is_empty())
            .or_else(|| {
                (arg == "-C")
                    .then(|| args.get(i + 1))
                    .flatten()
                    .and_then(|s| s.to_str())
            });
        if let Some(option) = option
            && option != own
            && let Some(linker) = option.strip_prefix("linker=")
        {
            selected = linker.into();
        }
    }
    Ok(selected)
}

pub fn value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.windows(2)
        .find(|a| a[0] == flag)
        .map(|a| a[1].as_str())
}

pub fn fat_link_args(
    args: &[String],
    target: &Path,
    archive: &Path,
    platform: Platform,
) -> Result<Vec<String>> {
    let mut builder = ar::Builder::new(fs::File::create(archive)?);
    let mut retained = std::collections::HashSet::new();
    let mut has_objects = false;
    for path in args.iter().filter(|arg| arg.ends_with(".rlib")) {
        if !Path::new(path).starts_with(target) {
            retained.insert(path.clone());
            continue;
        }
        let mut reader = ar::Archive::new(fs::File::open(path)?);
        while let Some(entry) = reader.next_entry() {
            let entry = entry?;
            let name = std::str::from_utf8(entry.header().identifier())?;
            if name.ends_with(".rmeta") || entry.header().size() == 0 {
                continue;
            }
            if !name.ends_with(".rcgu.o") {
                retained.insert(path.clone());
                continue;
            }
            has_objects = true;
            builder.append(&entry.header().clone(), entry)?;
        }
    }
    builder.into_inner()?;
    let mut output = Vec::new();
    let mut inserted = false;
    for arg in args {
        if arg.ends_with(".rlib") {
            if !inserted && has_objects {
                match platform {
                    Platform::Linux => output.extend([
                        "-Wl,--whole-archive".into(),
                        archive.display().to_string(),
                        "-Wl,--no-whole-archive".into(),
                    ]),
                    Platform::Macos => {
                        output.extend(["-Wl,-force_load".into(), archive.display().to_string()])
                    }
                }
                inserted = true;
            }
            if !retained.contains(arg) {
                continue;
            }
        }
        if matches!(arg.as_str(), "-Wl,--gc-sections" | "-Wl,-dead_strip") {
            continue;
        }
        output.push(arg.clone());
    }
    output.push(
        match platform {
            Platform::Linux => "-Wl,--export-dynamic-symbol,main",
            Platform::Macos => "-Wl,-exported_symbol,_main",
        }
        .into(),
    );
    Ok(output)
}

pub fn thin_link_args(args: &[String], platform: Platform) -> Vec<String> {
    let mut out: Vec<String> = match platform {
        Platform::Linux => [
            "-shared",
            "-Wl,--eh-frame-hdr",
            "-Wl,-z,noexecstack",
            "-Wl,-z,relro,-z,now",
            "-nodefaultlibs",
            "-Wl,-Bdynamic",
            "-Wl,-z,defs",
        ]
        .map(str::to_owned)
        .to_vec(),
        Platform::Macos => ["-dynamiclib", "-Wl,-undefined,error"]
            .map(str::to_owned)
            .to_vec(),
    };
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if arg == "-L"
            || (platform == Platform::Macos
                && matches!(
                    arg.as_str(),
                    "-F" | "-framework"
                        | "-weak_framework"
                        | "-arch"
                        | "-target"
                        | "--target"
                        | "-isysroot"
                        | "-isystem"
                ))
        {
            out.push(arg.clone());
            if let Some(path) = args.next() {
                out.push(path.clone());
            }
        } else if arg.starts_with("-l")
            || arg.starts_with("-L")
            || arg.starts_with("-m")
            || arg.starts_with("-B")
            || arg.starts_with("-fuse-ld")
            || arg.contains("-ld-path")
            || arg.starts_with("-Wl,--target=")
            || arg.starts_with("-Wl,-fuse-ld")
            || arg.ends_with(".so")
            || (platform == Platform::Macos
                && (arg.starts_with("-F")
                    || arg.ends_with(".dylib")
                    || arg.starts_with("--target=")
                    || arg.starts_with("-Wl,-syslibroot,")
                    || arg.starts_with("-Wl,-rpath,")
                    || arg.starts_with("-Wl,-platform_version,")))
        {
            out.push(arg.clone());
        }
    }
    out
}

pub fn scripts(session: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let exe = std::env::current_exe()?;
        let exe = exe.to_str().context("non-UTF-8 gpui executable path")?;
        for (file, mode) in [("rustc-wrapper", "__rustc"), ("linker", "__link")] {
            let path = session.join(file);
            let quoted = shell_words::quote(exe);
            fs::write(&path, format!("#!/bin/sh\nexec {quoted} {mode} \"$@\"\n"))?;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }
    #[cfg(not(unix))]
    anyhow::bail!("native watch currently requires Linux or macOS")
}

/// Each compiler/app has its own process group, so cancelling watch also stops
/// linker/compiler grandchildren. Dropping this guard always ends the group.
pub struct Process {
    child: std::process::Child,
    group: u32,
}

impl Process {
    pub fn spawn(command: &mut tokio::process::Command) -> Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.as_std_mut().process_group(0);
        }
        let child = command.as_std_mut().spawn()?;
        let group = child.id();
        Ok(Self { child, group })
    }
    pub fn id(&self) -> u32 {
        self.group
    }
    pub async fn wait(&mut self) -> Result<std::process::ExitStatus> {
        loop {
            if let Some(status) = self.child.try_wait()? {
                return Ok(status);
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        #[cfg(unix)]
        unsafe {
            libc::kill(-(self.group as i32), libc::SIGKILL);
        }
        let _ = self.child.wait();
    }
}

pub async fn run(command: &mut tokio::process::Command) -> Result<()> {
    let mut process = Process::spawn(command)?;
    let status = process.wait().await?;
    ensure!(status.success(), "compiler exited with {status}");
    Ok(())
}

pub async fn patch(
    session: &Path,
    cache: &crate::native::Cache,
    runtime_main: u64,
    sequence: u64,
) -> Result<PathBuf> {
    let capture: RustcCapture = serde_json::from_slice(&fs::read(session.join("rustc.json"))?)?;
    let cwd = capture.cwd;
    let mut cmd = tokio::process::Command::new(capture.compiler);
    let args = capture.args.into_iter().filter(|arg| {
        let arg = arg.to_string_lossy();
        !arg.starts_with("--error-format=") && !arg.starts_with("--json=")
    });
    cmd.args(args)
        .arg("--error-format=human")
        .current_dir(&cwd)
        .env_clear()
        .envs(capture.env)
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .env_remove("RUSTC_WRAPPER")
        .env_remove("CARGO_MAKEFLAGS")
        .env_remove("MAKEFLAGS")
        .env_remove("MFLAGS")
        .env("GPUI_LINK_MODE", "thin");
    // Cargo's captured linker configuration is reused by the internal linker.
    let link: LinkCapture = serde_json::from_slice(&fs::read(session.join("link.json"))?)?;
    cmd.env("GPUI_REAL_LINKER", &link.linker);
    run(&mut cmd).await.context("compile patch")?;
    let link: LinkCapture = serde_json::from_slice(&fs::read(session.join("link.json"))?)?;
    let mut objects: Vec<PathBuf> = link
        .args
        .iter()
        .filter(|a| a.ends_with(".rcgu.o"))
        .map(PathBuf::from)
        .collect();
    ensure!(!objects.is_empty(), "compiler produced no patch objects");
    let stub = session.join("stub.o");
    fs::write(&stub, cache.stub(&objects, runtime_main)?)?;
    objects.push(stub);
    let output = session.join(format!(
        "patch-{sequence}.{}",
        cache.platform.library_extension()
    ));
    let mut linker = tokio::process::Command::new(link.linker);
    linker
        .current_dir(&cwd)
        .args(&objects)
        .args(thin_link_args(&link.args, cache.platform))
        .arg("-o")
        .arg(&output);
    run(&mut linker).await.context("link patch")?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn thin_link_preserves_native_search_paths_and_omits_baseline_rlibs() {
        let args = [
            "tip.rcgu.o",
            "deps.rlib",
            "-L",
            "/native",
            "-L/other",
            "-lssl",
            "-lc",
            "-B/rust-linker",
            "-fuse-ld=lld",
            "-pie",
            "-o",
            "old-app",
        ]
        .map(str::to_owned);
        let actual = thin_link_args(&args, Platform::Linux);
        for required in [
            "-shared",
            "-Wl,-z,defs",
            "-L",
            "/native",
            "-L/other",
            "-lssl",
            "-lc",
            "-B/rust-linker",
            "-fuse-ld=lld",
        ] {
            assert!(actual.contains(&required.to_owned()), "missing {required}");
        }
        for omitted in ["tip.rcgu.o", "deps.rlib", "-pie", "-o", "old-app"] {
            assert!(!actual.contains(&omitted.to_owned()));
        }
    }

    #[test]
    fn macho_link_preserves_framework_sdk_and_architecture_arguments() {
        let args = [
            "tip.rcgu.o",
            "deps.rlib",
            "-framework",
            "CoreFoundation",
            "-weak_framework",
            "Metal",
            "-arch",
            "arm64",
            "-isysroot",
            "/sdk",
            "-target",
            "arm64-apple-macos13",
            "-L",
            "/libs",
            "-F/Frameworks",
            "-mmacosx-version-min=13.0",
            "-lSystem",
            "/libs/native.dylib",
            "-Wl,-dead_strip",
            "-Wl,-exported_symbol,_main",
            "-o",
            "app",
        ]
        .map(str::to_owned);
        let actual = thin_link_args(&args, Platform::Macos);
        assert_eq!(
            actual,
            [
                "-dynamiclib",
                "-Wl,-undefined,error",
                "-framework",
                "CoreFoundation",
                "-weak_framework",
                "Metal",
                "-arch",
                "arm64",
                "-isysroot",
                "/sdk",
                "-target",
                "arm64-apple-macos13",
                "-L",
                "/libs",
                "-F/Frameworks",
                "-mmacosx-version-min=13.0",
                "-lSystem",
                "/libs/native.dylib"
            ]
            .map(str::to_owned)
        );
    }

    #[test]
    fn fat_archive_keeps_native_members_in_original_rlib() {
        use std::io::Read;
        let dir = std::env::temp_dir().join(format!("gpui-archive-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let rlib = dir.join("libdep.rlib");
        let mut builder = ar::Builder::new(fs::File::create(&rlib).unwrap());
        for (name, bytes) in [
            ("dep.rcgu.o", b"rust".as_slice()),
            ("native.o", b"native".as_slice()),
            ("lib.rmeta", b"metadata".as_slice()),
        ] {
            builder
                .append(
                    &ar::Header::new(name.as_bytes().to_vec(), bytes.len() as u64),
                    bytes,
                )
                .unwrap();
        }
        builder.into_inner().unwrap();
        let archive = dir.join("deps.a");
        let args = vec![
            "tip.rcgu.o".to_owned(),
            rlib.display().to_string(),
            "-Wl,--gc-sections".to_owned(),
            "-o".to_owned(),
            "app".to_owned(),
        ];
        let mac_args: Vec<_> = args
            .iter()
            .cloned()
            .chain(["-Wl,-dead_strip".to_owned()])
            .collect();
        let mac_output =
            fat_link_args(&mac_args, &dir, &dir.join("mac.a"), Platform::Macos).unwrap();
        assert!(mac_output.contains(&"-Wl,-force_load".to_owned()));
        assert!(mac_output.contains(&"-Wl,-exported_symbol,_main".to_owned()));
        assert!(!mac_output.contains(&"-Wl,-dead_strip".to_owned()));
        assert!(!mac_output.contains(&"-Wl,--whole-archive".to_owned()));
        let output = fat_link_args(&args, &dir, &archive, Platform::Linux).unwrap();
        assert!(output.contains(&rlib.display().to_string()));
        assert!(output.contains(&"-Wl,--whole-archive".to_owned()));
        assert!(!output.contains(&"-Wl,--gc-sections".to_owned()));
        let mut archive = ar::Archive::new(fs::File::open(archive).unwrap());
        let mut entry = archive.next_entry().unwrap().unwrap();
        assert_eq!(entry.header().identifier(), b"dep.rcgu.o");
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"rust");
        drop(entry);
        assert!(archive.next_entry().is_none());
        fs::remove_dir_all(dir).unwrap();
    }
}
