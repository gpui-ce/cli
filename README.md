# GPUI-CE CLI

`gpui watch` builds and runs a GPUI-CE application and drives Subsecond hot
patching. It owns the Cargo/rustc capture, native linking, file watcher, and local
WebSocket patch server. Installing or invoking the Dioxus CLI (`dx`) is not required.

## Setup

Requires Rust and a working native GPUI development environment on Linux or
macOS. macOS uses the Xcode Command Line Tools (`xcode-select --install`).
x86_64 and aarch64 native targets are implemented. Linux x86_64 has also been
validated with a GPUI-CE application on Wayland.

```sh
cargo install --path . --locked
```

Create a standalone counter app with hot-patching enabled:

```sh
gpui new my-app
cd my-app
gpui watch
```

`gpui new PATH --name package-name` supports a different directory and package
name. It refuses to overwrite existing directories. The app includes its source,
Cargo manifest, README, Apache-2.0 license, and `.gitignore`. Both GPUI dependencies
are pinned to a compatible revision of main. macOS applications require full
Xcode with the Metal compiler, in addition to the CLI's Command Line Tools.
See [GPUI-CE's setup guide](https://github.com/gpui-ce/gpui-ce/blob/main/SETUP.md)
for platform dependencies.

For an existing application, enable runtime integration on your application's GPUI-CE dependency:

```toml
gpui-ce = { git = "https://github.com/gpui-ce/gpui-ce", features = ["hot-patching"] }
```

Use a revision containing the `hot-patching` feature. GPUI-CE connects to the
patch server on startup and redraws windows after patches. No extra
application-side connection is needed. The current protocol requires Subsecond
0.7.10, matching GPUI-CE main.

```sh
gpui watch
gpui watch -p my-app --bin my-app --features hot-patching
gpui watch --manifest-path ../my-app/Cargo.toml --locked
gpui watch --example demo --offline
gpui watch -- --application-flag 'argument with spaces'
gpui watch --args '--application-flag "argument with spaces"'
```

In a virtual workspace, select the application using `-p`. `--bin` and
`--example` are mutually exclusive. Repeated `--features`, `--all-features`,
`--no-default-features`, `--offline`, and `--locked` are supported. Features
apply to the application package; dependency features already enabled in its
manifest do not need to be repeated.

`--linker` overrides the C compiler driver used for linking. Otherwise the
captured Cargo linker is used, falling back to `cc`. `--port` selects the local
patch-server port, with an available port chosen by default. `--debounce-ms`
controls save stabilization, defaulting to 200 ms. Ctrl+C stops the application
and active compiler process groups.

## Behavior and limitations

The driver uses a separate `target/gpui` directory and a debug `gpui-dev`
profile. Initial linking includes dependency code needed by later patches.
Session artifacts, including compiler environment captures, are stored in
private session directories beneath that target directory. They remain after
shutdown for diagnosis and can be removed when no watch process is running.

Changes to application Rust code compile into shared libraries and are delivered
to the running process. Subsecond requires a hot-call boundary around code that
should change. GPUI-CE supplies boundaries for rendering, layout, prepaint, and
paint. Syntax and type errors leave the running application intact; the next
valid save retries. Content polling supplements filesystem events to catch
missed notifications and saves made during compilation.

Manifest, lockfile, build-script, shader, and local dependency changes cause a
full rebuild and restart. Packages that contain their own library target also
use full rebuilds. A failed rebuild preserves the current app until a valid
save succeeds. Source and configuration files in local dependency and workspace
directories are watched; arbitrary assets and external generated inputs are
not handled yet.

Hot patching is experimental. Restart watch after changing state layouts,
function signatures, static initialization, or code outside hot-call boundaries.
The driver does not detect incompatible layout changes. Linux ELF and macOS Mach-O on x86_64 and aarch64 are implemented. macOS
preserves frameworks, SDK paths, architecture and deployment-target flags and
loads patches as dylibs. Patches get fresh thread-local storage initialized from
the baseline; TLS state is not preserved across patches. Cross compilation,
Windows, web, and mobile are not supported. Custom linker configurations may
need additional handling.

See [Subsecond's runtime documentation](https://docs.rs/subsecond/0.7.10/subsecond/)
for hot-call semantics and limitations. The native symbol and linking algorithms
are adapted from [Dioxus CLI source](https://github.com/DioxusLabs/dioxus/tree/57d6794/packages/cli/src/build)
under its Apache-2.0 option; provenance is recorded in [NOTICE](NOTICE).
GPUI-CE still uses Dioxus devtools runtime crates for the wire protocol.

## Development

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo fetch --locked --manifest-path tests/fixtures/hot-patch/Cargo.toml
cargo test --locked --test watch -- --ignored --nocapture
# Requires a working display and GPUI native dependencies:
cargo test --locked --test gpui_window -- --ignored --nocapture
```

The explicit ignored test builds a real Subsecond application and opens a local
WebSocket server. It checks state-preserving patches, incomplete saves, compiler
errors, failed full rebuilds, restarts, application argument handling, and process
cleanup. It needs runtime dependencies available to Cargo in offline mode.

The native driver was also tested with a separate GPUI application using
GPUI-CE main at `96a5ecca405b288a0107eaa1c057d954db6a41be` on Linux x86_64 with
Wayland. Render patches preserved the PID, startup timestamp, and render counter.
That manual test requires a working display and GPUI's native dependencies.

GitHub Actions runs formatting, Clippy, unit tests, and the real hot-patch/restart
regression on Ubuntu, Apple Silicon macOS (`macos-15`), and Intel macOS
(`macos-15-intel`). The macOS fixture also calls CoreFoundation to exercise
framework linking. Both dependency graphs are locked and fetched before offline
checks. Separate macOS jobs build the actual generated counter app, open its
GPUI window, patch rendering twice, and verify that its PID and counter survive.
These use a checkout of the template's pinned GPUI-CE revision; locally,
`GPUI_SOURCE=/path/to/gpui-ce` can select an existing checkout. Failed jobs retain runtime logs for diagnosis.

## License

Licensed under the [Apache License, Version 2.0](LICENSE).
