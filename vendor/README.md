# Vendored crates

## `gpui-pre-windows` 0.3.12

Patched so folder-guard can be **release** cross-compiled from Linux to
`x86_64-pc-windows-gnu` without `fxc.exe`.

Upstream `build.rs` only compiles HLSL when the *host* is Windows and the
profile is release, then the library `include!`s `OUT_DIR/shaders_bytes.rs`.
On Linux that file is never produced, so a `--release` build fails.

This copy:

- inlines `alpha_correction.hlsl` into `src/shaders.hlsl`
- compiles those shaders at runtime on Windows with `D3DCompile`
- leaves `build.rs` as a no-op

Pinned by `[patch.crates-io]` in the workspace `Cargo.toml`.
