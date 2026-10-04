# GPUI-CE CLI

`gpui watch` builds and runs a GPUI-CE application, watches for edits, and drives
Subsecond hot patching. This first version uses Dioxus CLI 0.7.10 as its compiler,
watcher, and patch server. The `gpui` binary does not embed a separate compiler.

## Setup

```sh
cargo install --path . --locked
cargo install dioxus-cli --version 0.7.10 --locked
```

Enable the runtime integration on your application's GPUI-CE dependency:

```toml
gpui-ce = { git = "https://github.com/gpui-ce/gpui-ce", features = ["hot-patching"] }
```

Use a GPUI-CE revision containing the `hot-patching` feature. It connects to the
patch server during application startup and redraws windows when patches arrive.
No extra application-side connection is needed with this integration.

From the application's Cargo directory:

```sh
gpui watch
gpui watch -p my-app --bin my-app --features hot-patching
gpui watch --manifest-path ../my-app/Cargo.toml --locked
```

`--features` enables features on the application package. If GPUI-CE's feature is
already enabled in its dependency declaration, you do not need that argument.
In a workspace, select the application using `-p`. `--bin` and `--example` are
mutually exclusive. Repeated `--features`, `--all-features`,
`--no-default-features`, `--offline`, and `--locked` are supported.

Pass application arguments using the backend's argument string:

```sh
gpui watch --args 'hello world'
```

Use `--dx /path/to/dx` or `GPUI_DX` to select the backend executable. The CLI
checks its version against the runtime's 0.7.10 protocol. It starts
`dx serve --hot-patch --watch=true --open=false` with the selected build options.
On Unix, the backend replaces the CLI process so terminal input, Ctrl+C, and
exit status are preserved. On other platforms, the CLI waits for the backend.

## Scope

Hot patching is experimental and requires native debug builds. GPUI-CE hooks
rendering, layout, prepaint, and paint. Changes to other code, state layouts,
dependency versions, or static initializers may require restarting the app.
The initial build can be slower because it prepares the hot-patching artifacts.

GPUI-CE's current documentation reports a workspace dependency cycle error when
DX 0.7.10 patches examples directly inside the GPUI-CE workspace. Use a separate
application depending on GPUI-CE for that workflow.

See [Subsecond](https://docs.rs/subsecond/0.7.10/subsecond/) for runtime details
and limitations, and [the backend source](https://github.com/DioxusLabs/dioxus/tree/v0.7.10/packages/cli)
for compiler behavior.

## Development

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

The integration tests use a fake backend to verify argument forwarding, manifest
directory selection, version errors, process exit codes, and Unix signals. They
do not establish end-to-end GPUI hot-patching behavior, which requires DX and a
GPUI-CE application with a working native graphics environment.

## License

Licensed under the [Apache License, Version 2.0](LICENSE).
