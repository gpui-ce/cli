Develop GPUI-CE apps with Subsecond hot-patching, without installing Dioxus CLI.

- `gpui new my-app` generates a standalone counter app with hot-patching enabled.
- `gpui watch` builds, opens, watches, and patches the app.
- Native Linux x86_64, Apple Silicon macOS, and Intel macOS archives are included.
- SHA256SUMS covers all archives and the Cargo source package.

Extract the archive for your platform and put `gpui` on PATH. Rust and native
GPUI development dependencies are still required to build applications.
Linux binaries require glibc 2.39 or newer. macOS apps require Xcode with Metal.
The macOS CLI binaries are not Developer ID signed or notarized.

Hot-patching is experimental. Restart after changing state layouts or function
signatures. Thread-local state resets on patches. Windows and cross compilation
are not supported. Linux aarch64 currently requires building from source.
