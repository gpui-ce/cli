use anyhow::{Context, Result, ensure};
use clap::Args;
use std::{fs, path::PathBuf};

pub const GPUI_REV: &str = "96a5ecca405b288a0107eaa1c057d954db6a41be";

#[derive(Args, Debug)]
pub struct New {
    /// Directory to create (must not already exist)
    pub path: PathBuf,
    /// Cargo package name (defaults to the directory name)
    #[arg(long)]
    pub name: Option<String>,
}

pub fn run(options: New) -> Result<()> {
    let name = options
        .name
        .as_deref()
        .or_else(|| options.path.file_name()?.to_str())
        .context("cannot infer the package name; use --name")?;
    validate_name(name)?;
    ensure!(
        !options.path.exists(),
        "destination {} already exists; choose a new directory",
        options.path.display()
    );
    let manifest = include_str!("../templates/app/Cargo.toml.template")
        .replace("{{name}}", name)
        .replace("{{gpui_rev}}", GPUI_REV);
    let readme = include_str!("../templates/app/README.md.template").replace("{{name}}", name);
    if let Some(parent) = options.path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent).context("create parent directory")?;
    }
    // create_dir also rejects a destination that appears after the preflight check.
    fs::create_dir(&options.path).context("create app directory")?;
    fs::create_dir(options.path.join("src"))?;
    for (path, contents) in [
        ("Cargo.toml", manifest.as_str()),
        ("src/main.rs", include_str!("../templates/app/src/main.rs")),
        ("README.md", readme.as_str()),
        ("LICENSE", include_str!("../LICENSE")),
        (".gitignore", "/target\n"),
    ] {
        fs::write(options.path.join(path), contents).with_context(|| format!("write {path}"))?;
    }
    eprintln!("Created {name} in {}", options.path.display());
    eprintln!(
        "Run: gpui watch --manifest-path {}",
        shell_words::quote(&options.path.join("Cargo.toml").to_string_lossy())
    );
    Ok(())
}

fn validate_name(name: &str) -> Result<()> {
    let mut chars = name.chars();
    ensure!(
        chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
        "package name must start with a letter or underscore and contain only ASCII letters, digits, hyphens, or underscores"
    );
    let crate_name = name.replace('-', "_");
    ensure!(
        !matches!(
            crate_name.as_str(),
            "as" | "async"
                | "await"
                | "break"
                | "const"
                | "continue"
                | "crate"
                | "dyn"
                | "else"
                | "enum"
                | "extern"
                | "false"
                | "fn"
                | "for"
                | "if"
                | "impl"
                | "in"
                | "let"
                | "loop"
                | "match"
                | "mod"
                | "move"
                | "mut"
                | "pub"
                | "ref"
                | "return"
                | "self"
                | "Self"
                | "static"
                | "struct"
                | "super"
                | "trait"
                | "true"
                | "type"
                | "unsafe"
                | "use"
                | "where"
                | "while"
                | "abstract"
                | "become"
                | "box"
                | "do"
                | "final"
                | "gen"
                | "macro"
                | "override"
                | "priv"
                | "try"
                | "typeof"
                | "unsized"
                | "virtual"
                | "yield"
                | "std"
                | "core"
                | "alloc"
                | "proc_macro"
                | "_"
        ),
        "package name {name:?} is reserved; choose another name"
    );
    Ok(())
}
